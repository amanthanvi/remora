//! Rust-owned remote-host pairing lifecycle.
//!
//! The module intentionally exposes user intents instead of protocol phases:
//! inspect a code, pair, reconnect, revoke, or forget. Current v1 wire parsing
//! is retained only as a compatibility decoder. A v1 host-wide bearer token is
//! never accepted as a v2 device grant; migration requires a fresh v2 offer.
//!
//! The full coordinator is instantiated by tests today and by the production
//! adapter once Remora Link v2 lands. Keep the migration seam compiled in the
//! interim without turning expected adapter-facing items into warning noise.
#![allow(dead_code)]

mod identity;
mod ports;
pub(crate) mod remora_link_v2;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use tokio::sync::Mutex;

use self::identity::{DecodedPairingCode, decode_pairing_code};
use self::ports::{
    HostConfirmReconnectRequest, HostEstablishRequest, HostInspectRequest, HostPortError,
    HostReconnectRequest, HostRevokeRequest, JournalError, JournalHostState, PairingClock,
    PairingIdSource, PairingJournalEntry, PairingJournalPort, PairingSecretPort,
    RemotePairingHostPort, SecretStoreError,
};
use self::types::{
    HostCredentialRevocationStatus, RemoteForgetOutcome, RemoteHostId, RemoteHostPairingError,
    RemotePairingAcceptance, RemotePairingCode, RemotePairingCodeInspection, RemotePairingOffer,
    RemotePairingOfferId, RemotePairingOutcome, RemotePairingProtocol, RemotePairingRepairReason,
    RemoteRePairReason, RemoteReconnectOutcome, RemoteRevokeOutcome, RemoteRuntimeOffer,
};

const LEGACY_OFFER_TTL_SECONDS: u64 = 5 * 60;
const MAX_CACHED_OFFERS: usize = 32;
const MAX_RUNTIME_CHOICES: usize = 64;
const MAX_RUNTIME_ID_CHARS: usize = 128;
const MAX_DISPLAY_NAME_CHARS: usize = 96;

/// Classify a QR/copy-paste code without exposing credential-bearing fields.
///
/// This performs strict local validation only. `RemoteHostPairing::inspect`
/// additionally authenticates v2 host metadata through the host port.
pub fn inspect_remote_pairing_code(
    code: RemotePairingCode,
) -> Result<RemotePairingCodeInspection, RemoteHostPairingError> {
    let now = ports::PairingClock::unix_seconds(&ports::SystemClock);
    decode_pairing_code(code.encoded, now).map(|decoded| decoded.inspection())
}

struct CachedOffer {
    public: RemotePairingOffer,
    /// Cleared after a successful commit so duplicate callers may observe the
    /// idempotent result without retaining invitation material.
    invite: Option<DecodedPairingCode>,
}

#[derive(Clone, Copy)]
enum CredentialDisposition {
    DefinitivelyInvalid,
    Quarantined,
}

/// Deep lifecycle coordinator. Production and test adapters implement its
/// private, semantic ports; mobile callers never receive those adapters or
/// their wire/storage types.
pub(crate) struct RemoteHostPairing {
    host: Arc<dyn RemotePairingHostPort>,
    journal: Arc<dyn PairingJournalPort>,
    secrets: Arc<dyn PairingSecretPort>,
    clock: Arc<dyn PairingClock>,
    ids: Arc<dyn PairingIdSource>,
    offers: Mutex<HashMap<String, CachedOffer>>,
    host_locks: Mutex<HashMap<RemoteHostId, Weak<Mutex<()>>>>,
}

impl RemoteHostPairing {
    pub(crate) fn new(
        host: Arc<dyn RemotePairingHostPort>,
        journal: Arc<dyn PairingJournalPort>,
        secrets: Arc<dyn PairingSecretPort>,
        clock: Arc<dyn PairingClock>,
        ids: Arc<dyn PairingIdSource>,
    ) -> Self {
        Self {
            host,
            journal,
            secrets,
            clock,
            ids,
            offers: Mutex::new(HashMap::new()),
            host_locks: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn inspect(
        &self,
        code: RemotePairingCode,
    ) -> Result<RemotePairingOffer, RemoteHostPairingError> {
        let now = self.clock.unix_seconds();
        let decoded = decode_pairing_code(code.encoded, now)?;
        let inspection = decoded.inspection();

        let (runtimes, display_name) = match decoded.protocol() {
            RemotePairingProtocol::LegacyV1 => {
                // Do not transmit the v1 bearer while inspecting the new v2
                // workflow. Existing saved sessions remain on their dedicated
                // compatibility path; new authority requires a v2 offer.
                (Vec::new(), inspection.suggested_display_name.clone())
            }
            RemotePairingProtocol::DeviceGrantV2 => {
                let authenticated = self
                    .host
                    .inspect(HostInspectRequest {
                        invite: decoded.clone(),
                    })
                    .await
                    .map_err(map_host_error)?;
                if authenticated.host_id != inspection.host_id {
                    return Err(RemoteHostPairingError::ProtocolViolation);
                }
                (
                    normalize_runtime_offers(authenticated.runtimes)?,
                    normalize_display_name(
                        Some(authenticated.suggested_display_name),
                        &inspection.suggested_display_name,
                    ),
                )
            }
        };

        let expires_at_seconds = decoded
            .expires_at_unix_ms()
            .and_then(|millis| millis.checked_div(1_000))
            .unwrap_or_else(|| now.saturating_add(LEGACY_OFFER_TTL_SECONDS));
        if expires_at_seconds <= now {
            return Err(RemoteHostPairingError::OfferExpired);
        }
        let offer_id = RemotePairingOfferId {
            value: self.ids.next_id("offer"),
        };
        let offer = RemotePairingOffer {
            offer_id: offer_id.clone(),
            host_id: inspection.host_id,
            suggested_display_name: display_name,
            protocol: inspection.protocol,
            disposition: inspection.disposition,
            runtimes,
            expires_at_unix_ms: expires_at_seconds.saturating_mul(1_000),
        };

        let mut offers = self.offers.lock().await;
        offers.retain(|_, cached| cached.public.expires_at_unix_ms / 1_000 > now);
        if offers.len() >= MAX_CACHED_OFFERS {
            let oldest = offers
                .iter()
                .min_by_key(|(_, cached)| cached.public.expires_at_unix_ms)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                offers.remove(&oldest);
            }
        }
        offers.insert(
            offer_id.value.clone(),
            CachedOffer {
                public: offer.clone(),
                // A legacy bearer is useful only to classify the migration
                // requirement. Do not retain it in the v2 acceptance cache.
                invite: match offer.protocol {
                    RemotePairingProtocol::LegacyV1 => None,
                    RemotePairingProtocol::DeviceGrantV2 => Some(decoded),
                },
            },
        );
        Ok(offer)
    }

    pub(crate) async fn pair(
        &self,
        acceptance: RemotePairingAcceptance,
    ) -> Result<RemotePairingOutcome, RemoteHostPairingError> {
        let offer = {
            let offers = self.offers.lock().await;
            let cached = offers
                .get(&acceptance.offer_id.value)
                .ok_or(RemoteHostPairingError::UnknownOffer)?;
            cached.public.clone()
        };
        let host_lock = self.host_lock(&offer.host_id).await;
        let _operation = host_lock.lock().await;

        // Revocation invalidates offers while waiting on the host lock. Recheck
        // under the serialized host operation before using invitation material.
        let invite = {
            let offers = self.offers.lock().await;
            match offers.get(&acceptance.offer_id.value) {
                Some(cached) => cached.invite.clone(),
                None => {
                    return self.idempotent_pair_result(&offer.host_id).await;
                }
            }
        };
        let now = self.clock.unix_seconds();
        if offer.expires_at_unix_ms / 1_000 <= now {
            self.offers.lock().await.remove(&acceptance.offer_id.value);
            return Ok(RemotePairingOutcome::RePairRequired {
                host_id: offer.host_id,
                reason: RemoteRePairReason::OfferExpired,
            });
        }
        if offer.protocol == RemotePairingProtocol::LegacyV1 {
            return Ok(RemotePairingOutcome::RePairRequired {
                host_id: offer.host_id,
                reason: RemoteRePairReason::LegacyBearerCredential,
            });
        }
        let invite = match invite {
            Some(invite) => invite,
            None => return self.idempotent_pair_result(&offer.host_id).await,
        };
        let selected_runtime_ids =
            validate_runtime_selection(&offer.runtimes, acceptance.selected_runtime_ids)?;
        let display_name =
            normalize_display_name(acceptance.display_name, &offer.suggested_display_name);

        let mut previous = self.load_journal(&offer.host_id).await?;
        if let Some(entry) = previous.clone() {
            if let JournalHostState::EnrollmentRollbackPending(reason) = entry.state {
                self.resume_pairing_rollback(entry).await?;
                return Ok(RemotePairingOutcome::NeedsRepair {
                    host_id: offer.host_id,
                    reason,
                });
            }
            let cleanup_is_authoritative = matches!(
                entry.state,
                JournalHostState::EnrollmentRolledBack(_)
                    | JournalHostState::RePairRequired(RemoteRePairReason::CredentialRejected)
                    | JournalHostState::RePairRequired(RemoteRePairReason::Revoked)
            );
            if cleanup_is_authoritative && entry_has_credentials(&entry) {
                if !self.delete_entry_credentials(&entry).await {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::SecureStorageUnavailable,
                    });
                }
                let cleaned = PairingJournalEntry {
                    revision: entry.revision + 1,
                    credential_alias: String::new(),
                    pending_credential_alias: None,
                    operation_id: None,
                    state: JournalHostState::RePairRequired(RemoteRePairReason::MissingPairing),
                    ..entry.clone()
                };
                self.cas_journal(Some(&entry), cleaned.clone()).await?;
                previous = Some(cleaned);
            } else if matches!(entry.state, JournalHostState::EnrollmentRolledBack(_)) {
                let cleaned = PairingJournalEntry {
                    revision: entry.revision + 1,
                    operation_id: None,
                    state: JournalHostState::RePairRequired(RemoteRePairReason::MissingPairing),
                    ..entry.clone()
                };
                self.cas_journal(Some(&entry), cleaned.clone()).await?;
                previous = Some(cleaned);
            } else if matches!(entry.state, JournalHostState::RePairRequired(_))
                && entry_has_credentials(&entry)
            {
                // Identity/protocol failures do not prove the prior authority
                // was invalidated. Preserve it for explicit revoke or forget.
                return Ok(RemotePairingOutcome::NeedsRepair {
                    host_id: offer.host_id,
                    reason: RemotePairingRepairReason::HostCredentialNeedsRevocation,
                });
            }
        }
        if let Some(previous) = previous.as_ref() {
            match previous.state {
                JournalHostState::Active => {
                    self.consume_offer(&acceptance.offer_id).await;
                    return Ok(RemotePairingOutcome::AlreadyPaired {
                        host_id: offer.host_id,
                    });
                }
                JournalHostState::Revoking
                | JournalHostState::RevocationSettled { .. }
                | JournalHostState::NeedsRepair(RemotePairingRepairReason::InterruptedRevocation) =>
                {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::InterruptedRevocation,
                    });
                }
                JournalHostState::Forgetting { .. } => {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::SecureStorageUnavailable,
                    });
                }
                JournalHostState::NeedsRepair(
                    RemotePairingRepairReason::SecureStorageUnavailable,
                ) => {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::SecureStorageUnavailable,
                    });
                }
                JournalHostState::CommitPending => {
                    // A cancellation or process death may have happened after
                    // the host accepted this exact transaction. Only replay a
                    // byte-for-byte equivalent intent with the persisted key.
                    if previous.protocol != offer.protocol
                        || previous.display_name != display_name
                        || previous.desired_runtime_ids != selected_runtime_ids
                        || previous.credential_alias.is_empty()
                        || previous.operation_id.is_none()
                    {
                        return Ok(RemotePairingOutcome::NeedsRepair {
                            host_id: offer.host_id,
                            reason: RemotePairingRepairReason::InterruptedCommit,
                        });
                    }
                }
                JournalHostState::ReconnectPending
                | JournalHostState::ReconnectConfirmPending { .. }
                | JournalHostState::ReconnectSettled { .. }
                | JournalHostState::NeedsRepair(RemotePairingRepairReason::InterruptedReconnect) => {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::InterruptedReconnect,
                    });
                }
                JournalHostState::EnrollmentRollbackPending(_)
                | JournalHostState::EnrollmentRolledBack(_) => {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::SecureStorageUnavailable,
                    });
                }
                JournalHostState::NeedsRepair(_)
                | JournalHostState::Revoked
                | JournalHostState::Forgotten
                | JournalHostState::RePairRequired(_) => {
                    // A fresh v2 offer is the explicit re-pair/recovery act.
                }
            }
        }

        let (transaction_id, credential_alias, generation) = match previous.as_ref() {
            Some(previous) if previous.state == JournalHostState::CommitPending => {
                let Some(operation_id) = previous.operation_id.clone() else {
                    return Ok(RemotePairingOutcome::NeedsRepair {
                        host_id: offer.host_id,
                        reason: RemotePairingRepairReason::InterruptedCommit,
                    });
                };
                (
                    operation_id,
                    previous.credential_alias.clone(),
                    previous.generation,
                )
            }
            _ => (
                self.ids.next_id("pair"),
                self.ids.next_id("credential"),
                previous.as_ref().map_or(1, |entry| entry.generation + 1),
            ),
        };
        let pending = PairingJournalEntry {
            revision: previous.as_ref().map_or(1, |entry| entry.revision + 1),
            generation,
            host_id: offer.host_id.clone(),
            protocol: offer.protocol,
            display_name: display_name.clone(),
            desired_runtime_ids: selected_runtime_ids.clone(),
            credential_alias,
            pending_credential_alias: None,
            operation_id: Some(transaction_id.clone()),
            state: JournalHostState::CommitPending,
        };
        self.cas_journal(previous.as_ref(), pending.clone()).await?;

        let established = match self
            .host
            .establish(HostEstablishRequest {
                invite,
                display_name,
                selected_runtime_ids: selected_runtime_ids.clone(),
                idempotency_key: transaction_id.clone(),
            })
            .await
        {
            Ok(established) => established,
            Err(HostPortError::AuthenticationRejected) => {
                self.mark_needs_repair(pending, RemotePairingRepairReason::InterruptedCommit)
                    .await?;
                self.invalidate_offers(&offer.host_id).await;
                return Ok(RemotePairingOutcome::RePairRequired {
                    host_id: offer.host_id,
                    reason: RemoteRePairReason::CredentialRejected,
                });
            }
            Err(HostPortError::V2Unavailable) => {
                self.mark_needs_repair(pending, RemotePairingRepairReason::InterruptedCommit)
                    .await?;
                self.invalidate_offers(&offer.host_id).await;
                return Ok(RemotePairingOutcome::RePairRequired {
                    host_id: offer.host_id,
                    reason: RemoteRePairReason::V2HostProtocolUnavailable,
                });
            }
            Err(HostPortError::HostIdentityChanged) => {
                self.mark_needs_repair(pending, RemotePairingRepairReason::InterruptedCommit)
                    .await?;
                self.invalidate_offers(&offer.host_id).await;
                return Ok(RemotePairingOutcome::RePairRequired {
                    host_id: offer.host_id,
                    reason: RemoteRePairReason::HostIdentityChanged,
                });
            }
            Err(HostPortError::CredentialRevoked) => {
                self.mark_needs_repair(pending, RemotePairingRepairReason::InterruptedCommit)
                    .await?;
                self.invalidate_offers(&offer.host_id).await;
                return Ok(RemotePairingOutcome::RePairRequired {
                    host_id: offer.host_id,
                    reason: RemoteRePairReason::Revoked,
                });
            }
            Err(error @ (HostPortError::Unavailable | HostPortError::Cancelled)) => {
                // Delivery may have failed after the host committed. Preserve
                // CommitPending and the original idempotency key for replay.
                return Err(map_host_error(error));
            }
            Err(HostPortError::ProtocolViolation) => {
                self.rollback_pairing_transaction(
                    &offer.host_id,
                    &transaction_id,
                    &pending,
                    RemotePairingRepairReason::InterruptedCommit,
                )
                .await?;
                return Err(RemoteHostPairingError::ProtocolViolation);
            }
        };

        if !valid_connected_runtime_set(&selected_runtime_ids, &established.connected_runtime_ids) {
            self.rollback_pairing_transaction(
                &offer.host_id,
                &transaction_id,
                &pending,
                RemotePairingRepairReason::InterruptedCommit,
            )
            .await?;
            return Err(RemoteHostPairingError::NoRuntimeConnected);
        }
        let credential = match established.credential {
            Some(credential) if !credential.is_empty() => credential,
            _ => {
                self.rollback_pairing_transaction(
                    &offer.host_id,
                    &transaction_id,
                    &pending,
                    RemotePairingRepairReason::InterruptedCommit,
                )
                .await?;
                return Err(RemoteHostPairingError::ProtocolViolation);
            }
        };

        if self
            .secrets
            .write(&pending.credential_alias, credential)
            .await
            .is_err()
        {
            self.rollback_pairing_transaction(
                &offer.host_id,
                &transaction_id,
                &pending,
                RemotePairingRepairReason::SecureStorageUnavailable,
            )
            .await?;
            return Ok(RemotePairingOutcome::NeedsRepair {
                host_id: offer.host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }

        let active = PairingJournalEntry {
            revision: pending.revision + 1,
            operation_id: None,
            state: JournalHostState::Active,
            ..pending.clone()
        };
        if self
            .journal
            .compare_and_swap(&offer.host_id, Some(pending.revision), active)
            .await
            .is_err()
        {
            // The durable CommitPending record and stored credential are a
            // recoverable transaction. Do not orphan host authority by rolling
            // it back or deleting the only local credential on an ambiguous CAS.
            return Ok(RemotePairingOutcome::NeedsRepair {
                host_id: offer.host_id,
                reason: RemotePairingRepairReason::JournalUnavailable,
            });
        }

        self.consume_offer(&acceptance.offer_id).await;
        Ok(RemotePairingOutcome::Paired {
            host_id: offer.host_id,
        })
    }

    pub(crate) async fn reconnect(
        &self,
        host_id: RemoteHostId,
    ) -> Result<RemoteReconnectOutcome, RemoteHostPairingError> {
        let host_lock = self.host_lock(&host_id).await;
        let _operation = host_lock.lock().await;
        let current = match self.load_journal(&host_id).await? {
            Some(entry) => entry,
            None => {
                return Ok(RemoteReconnectOutcome::RePairRequired {
                    host_id,
                    reason: RemoteRePairReason::MissingPairing,
                });
            }
        };
        match current.state {
            JournalHostState::Revoked => {
                return Ok(RemoteReconnectOutcome::RePairRequired {
                    host_id,
                    reason: RemoteRePairReason::Revoked,
                });
            }
            JournalHostState::Forgotten => {
                return Ok(RemoteReconnectOutcome::RePairRequired {
                    host_id,
                    reason: RemoteRePairReason::MissingPairing,
                });
            }
            JournalHostState::CommitPending => {
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::InterruptedCommit,
                });
            }
            JournalHostState::EnrollmentRollbackPending(_)
            | JournalHostState::EnrollmentRolledBack(_) => {
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::SecureStorageUnavailable,
                });
            }
            JournalHostState::Revoking | JournalHostState::RevocationSettled { .. } => {
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::InterruptedRevocation,
                });
            }
            JournalHostState::Forgetting { .. } => {
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::SecureStorageUnavailable,
                });
            }
            JournalHostState::NeedsRepair(reason) => {
                return Ok(RemoteReconnectOutcome::NeedsRepair { host_id, reason });
            }
            JournalHostState::RePairRequired(reason) => {
                return Ok(RemoteReconnectOutcome::RePairRequired { host_id, reason });
            }
            JournalHostState::ReconnectPending => {
                return self.resume_reconnect(current).await;
            }
            JournalHostState::ReconnectConfirmPending { already_connected } => {
                return self.confirm_reconnect(current, already_connected).await;
            }
            JournalHostState::ReconnectSettled { already_connected } => {
                return self.finish_reconnect(current, already_connected).await;
            }
            JournalHostState::Active => {}
        }
        if current.protocol == RemotePairingProtocol::LegacyV1 {
            return Ok(RemoteReconnectOutcome::RePairRequired {
                host_id,
                reason: RemoteRePairReason::LegacyBearerCredential,
            });
        }

        if self.read_credential(&current).await?.is_none() {
            self.mark_needs_repair(current, RemotePairingRepairReason::MissingHostCredential)
                .await?;
            return Ok(RemoteReconnectOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::MissingHostCredential,
            });
        }

        let pending = PairingJournalEntry {
            revision: current.revision + 1,
            pending_credential_alias: Some(self.ids.next_id("credential")),
            operation_id: Some(self.ids.next_id("reconnect")),
            state: JournalHostState::ReconnectPending,
            ..current.clone()
        };
        self.cas_journal(Some(&current), pending.clone()).await?;
        self.resume_reconnect(pending).await
    }

    async fn resume_reconnect(
        &self,
        pending: PairingJournalEntry,
    ) -> Result<RemoteReconnectOutcome, RemoteHostPairingError> {
        let host_id = pending.host_id.clone();
        let operation_id = pending
            .operation_id
            .clone()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        let pending_alias = pending
            .pending_credential_alias
            .clone()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;

        // A secret may have been written immediately before the journal CAS
        // failed. Its presence is durable proof that the host returned a valid
        // staged rotation for this exact operation.
        if self
            .secrets
            .read(&pending_alias)
            .await
            .map_err(map_secret_error)?
            .is_some()
        {
            let confirming = PairingJournalEntry {
                revision: pending.revision + 1,
                state: JournalHostState::ReconnectConfirmPending {
                    already_connected: false,
                },
                ..pending.clone()
            };
            self.cas_journal(Some(&pending), confirming.clone()).await?;
            return self.confirm_reconnect(confirming, false).await;
        }

        let credential = match self.read_credential(&pending).await? {
            Some(credential) => credential,
            None => {
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::MissingHostCredential,
                });
            }
        };
        let established = match self
            .host
            .reconnect(HostReconnectRequest {
                host_id: host_id.clone(),
                credential,
                selected_runtime_ids: pending.desired_runtime_ids.clone(),
                idempotency_key: operation_id,
            })
            .await
        {
            Ok(established) => established,
            Err(error) => return self.handle_reconnect_host_error(pending, error).await,
        };
        if !valid_connected_runtime_set(
            &pending.desired_runtime_ids,
            &established.connected_runtime_ids,
        ) {
            return Err(RemoteHostPairingError::ProtocolViolation);
        }

        if let Some(credential) = established.credential {
            if credential.is_empty() {
                return Err(RemoteHostPairingError::ProtocolViolation);
            }
            if self
                .secrets
                .write(&pending_alias, credential)
                .await
                .is_err()
            {
                // ReconnectPending and its operation key remain replayable. The
                // old credential is still authoritative until confirmation.
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::SecureStorageUnavailable,
                });
            }
            let already_connected = established.already_connected;
            let confirming = PairingJournalEntry {
                revision: pending.revision + 1,
                state: JournalHostState::ReconnectConfirmPending { already_connected },
                ..pending.clone()
            };
            self.cas_journal(Some(&pending), confirming.clone()).await?;
            return self.confirm_reconnect(confirming, already_connected).await;
        }

        let active = PairingJournalEntry {
            revision: pending.revision + 1,
            pending_credential_alias: None,
            operation_id: None,
            state: JournalHostState::Active,
            ..pending.clone()
        };
        self.cas_journal(Some(&pending), active).await?;
        Ok(reconnect_success(host_id, established.already_connected))
    }

    async fn confirm_reconnect(
        &self,
        confirming: PairingJournalEntry,
        already_connected: bool,
    ) -> Result<RemoteReconnectOutcome, RemoteHostPairingError> {
        let host_id = confirming.host_id.clone();
        let operation_id = confirming
            .operation_id
            .clone()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        let pending_alias = confirming
            .pending_credential_alias
            .as_deref()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        let credential = match self
            .secrets
            .read(pending_alias)
            .await
            .map_err(map_secret_error)?
        {
            Some(credential) => credential,
            None => {
                // Storage loss after the confirmation state was journaled can
                // be repaired by replaying the staged reconnect with its key.
                let replay = PairingJournalEntry {
                    revision: confirming.revision + 1,
                    state: JournalHostState::ReconnectPending,
                    ..confirming.clone()
                };
                self.cas_journal(Some(&confirming), replay).await?;
                return Ok(RemoteReconnectOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::InterruptedReconnect,
                });
            }
        };
        match self
            .host
            .confirm_reconnect(HostConfirmReconnectRequest {
                host_id: host_id.clone(),
                credential,
                idempotency_key: operation_id,
            })
            .await
        {
            Ok(()) => {}
            Err(error) => {
                return self.handle_reconnect_host_error(confirming, error).await;
            }
        }

        let settled = PairingJournalEntry {
            revision: confirming.revision + 1,
            state: JournalHostState::ReconnectSettled { already_connected },
            ..confirming.clone()
        };
        self.cas_journal(Some(&confirming), settled.clone()).await?;
        self.finish_reconnect(settled, already_connected).await
    }

    async fn finish_reconnect(
        &self,
        settled: PairingJournalEntry,
        already_connected: bool,
    ) -> Result<RemoteReconnectOutcome, RemoteHostPairingError> {
        let host_id = settled.host_id.clone();
        let pending_alias = settled
            .pending_credential_alias
            .clone()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        if self
            .secrets
            .read(&pending_alias)
            .await
            .map_err(map_secret_error)?
            .is_none()
        {
            return Ok(RemoteReconnectOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::MissingHostCredential,
            });
        }
        if !settled.credential_alias.is_empty()
            && settled.credential_alias != pending_alias
            && self
                .secrets
                .delete(&settled.credential_alias)
                .await
                .is_err()
        {
            return Ok(RemoteReconnectOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }
        let active = PairingJournalEntry {
            revision: settled.revision + 1,
            credential_alias: pending_alias,
            pending_credential_alias: None,
            operation_id: None,
            state: JournalHostState::Active,
            ..settled.clone()
        };
        self.cas_journal(Some(&settled), active).await?;
        Ok(reconnect_success(host_id, already_connected))
    }

    async fn handle_reconnect_host_error(
        &self,
        entry: PairingJournalEntry,
        error: HostPortError,
    ) -> Result<RemoteReconnectOutcome, RemoteHostPairingError> {
        let host_id = entry.host_id.clone();
        let (reason, disposition) = match error {
            HostPortError::AuthenticationRejected => (
                RemoteRePairReason::CredentialRejected,
                CredentialDisposition::DefinitivelyInvalid,
            ),
            HostPortError::CredentialRevoked => (
                RemoteRePairReason::Revoked,
                CredentialDisposition::DefinitivelyInvalid,
            ),
            HostPortError::HostIdentityChanged => (
                RemoteRePairReason::HostIdentityChanged,
                CredentialDisposition::Quarantined,
            ),
            HostPortError::V2Unavailable => (
                RemoteRePairReason::V2HostProtocolUnavailable,
                CredentialDisposition::Quarantined,
            ),
            HostPortError::Unavailable => {
                return Ok(RemoteReconnectOutcome::TemporarilyUnavailable { host_id });
            }
            HostPortError::Cancelled | HostPortError::ProtocolViolation => {
                return Err(map_host_error(error));
            }
        };
        if !self
            .transition_to_repair_required(entry, reason, disposition)
            .await?
        {
            return Ok(RemoteReconnectOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }
        Ok(RemoteReconnectOutcome::RePairRequired { host_id, reason })
    }

    pub(crate) async fn revoke(
        &self,
        host_id: RemoteHostId,
    ) -> Result<RemoteRevokeOutcome, RemoteHostPairingError> {
        let host_lock = self.host_lock(&host_id).await;
        let _operation = host_lock.lock().await;
        self.invalidate_offers(&host_id).await;
        let mut current = match self.load_journal(&host_id).await? {
            Some(entry) => entry,
            None => return Ok(RemoteRevokeOutcome::AlreadyRevoked { host_id }),
        };
        if current.state == JournalHostState::Revoked {
            return Ok(RemoteRevokeOutcome::AlreadyRevoked { host_id });
        }
        if current.state == JournalHostState::Forgotten {
            return Ok(RemoteRevokeOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::MissingHostCredential,
            });
        }
        if matches!(current.state, JournalHostState::Forgetting { .. }) {
            return Ok(RemoteRevokeOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }
        if current.state == JournalHostState::CommitPending {
            let Some(operation_id) = current.operation_id.clone() else {
                return Ok(RemoteRevokeOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::InterruptedCommit,
                });
            };
            if !self
                .rollback_pairing_transaction(
                    &host_id,
                    &operation_id,
                    &current,
                    RemotePairingRepairReason::InterruptedCommit,
                )
                .await?
            {
                return Ok(RemoteRevokeOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::InterruptedCommit,
                });
            }
            current = self
                .load_journal(&host_id)
                .await?
                .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        }
        if let JournalHostState::EnrollmentRollbackPending(reason) = current.state {
            if !self.resume_pairing_rollback(current.clone()).await? {
                return Ok(RemoteRevokeOutcome::NeedsRepair { host_id, reason });
            }
            current = self
                .load_journal(&host_id)
                .await?
                .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        }
        if matches!(current.state, JournalHostState::EnrollmentRolledBack(_)) {
            if !self.delete_entry_credentials(&current).await {
                return Ok(RemoteRevokeOutcome::NeedsRepair {
                    host_id,
                    reason: RemotePairingRepairReason::SecureStorageUnavailable,
                });
            }
            let revoked = PairingJournalEntry {
                revision: current.revision + 1,
                credential_alias: String::new(),
                pending_credential_alias: None,
                operation_id: None,
                state: JournalHostState::Revoked,
                ..current.clone()
            };
            self.cas_journal(Some(&current), revoked).await?;
            return Ok(RemoteRevokeOutcome::AlreadyRevoked { host_id });
        }
        if matches!(
            current.state,
            JournalHostState::RePairRequired(RemoteRePairReason::CredentialRejected)
                | JournalHostState::RePairRequired(RemoteRePairReason::Revoked)
        ) && !entry_has_credentials(&current)
        {
            return Ok(RemoteRevokeOutcome::AlreadyRevoked { host_id });
        }
        if matches!(current.state, JournalHostState::RePairRequired(_))
            && !entry_has_credentials(&current)
        {
            return Ok(RemoteRevokeOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::MissingHostCredential,
            });
        }

        let promote_pending_for_revoke = match current.state {
            JournalHostState::ReconnectConfirmPending { .. }
            | JournalHostState::ReconnectSettled { .. } => true,
            JournalHostState::RePairRequired(_) => {
                if let Some(alias) = current.pending_credential_alias.as_deref() {
                    self.secrets
                        .read(alias)
                        .await
                        .map_err(map_secret_error)?
                        .is_some()
                } else {
                    false
                }
            }
            _ => false,
        };

        let (settled, status) = match current.state {
            JournalHostState::RevocationSettled {
                host_credential_status,
            } => (current, host_credential_status),
            _ => {
                let reuse_operation = matches!(
                    current.state,
                    JournalHostState::Revoking
                        | JournalHostState::NeedsRepair(
                            RemotePairingRepairReason::InterruptedRevocation
                        )
                );
                let operation_id = if reuse_operation {
                    current
                        .operation_id
                        .clone()
                        .unwrap_or_else(|| self.ids.next_id("revoke"))
                } else {
                    self.ids.next_id("revoke")
                };
                let revoking = if current.state == JournalHostState::Revoking
                    && current.operation_id.as_deref() == Some(operation_id.as_str())
                {
                    current
                } else {
                    let mut next = PairingJournalEntry {
                        revision: current.revision + 1,
                        operation_id: Some(operation_id.clone()),
                        state: JournalHostState::Revoking,
                        ..current.clone()
                    };
                    if promote_pending_for_revoke {
                        let Some(authoritative_alias) = current.pending_credential_alias.clone()
                        else {
                            return Ok(RemoteRevokeOutcome::NeedsRepair {
                                host_id,
                                reason: RemotePairingRepairReason::MissingHostCredential,
                            });
                        };
                        next.pending_credential_alias = if current.credential_alias.is_empty() {
                            None
                        } else {
                            Some(current.credential_alias.clone())
                        };
                        next.credential_alias = authoritative_alias;
                    }
                    self.cas_journal(Some(&current), next.clone()).await?;
                    next
                };
                self.host.close_local(&host_id).await;
                let credential = match self.read_credential(&revoking).await? {
                    Some(credential) => credential,
                    None => {
                        self.mark_needs_repair(
                            revoking,
                            RemotePairingRepairReason::InterruptedRevocation,
                        )
                        .await?;
                        return Ok(RemoteRevokeOutcome::NeedsRepair {
                            host_id,
                            reason: RemotePairingRepairReason::InterruptedRevocation,
                        });
                    }
                };

                let status = if revoking.protocol == RemotePairingProtocol::LegacyV1 {
                    HostCredentialRevocationStatus::UnsupportedByLegacyProtocol
                } else {
                    match self
                        .host
                        .revoke(HostRevokeRequest {
                            host_id: host_id.clone(),
                            credential,
                            idempotency_key: operation_id,
                        })
                        .await
                    {
                        Ok(status) => status,
                        Err(HostPortError::CredentialRevoked) => {
                            HostCredentialRevocationStatus::Confirmed
                        }
                        Err(HostPortError::Unavailable) => HostCredentialRevocationStatus::Deferred,
                        Err(error) => return Err(map_host_error(error)),
                    }
                };

                if status == HostCredentialRevocationStatus::Deferred {
                    // Keep the credential only for a later host-authoritative
                    // revoke. Revoking already forbids reconnect.
                    return Ok(RemoteRevokeOutcome::Revoked {
                        host_id,
                        host_credential_status: status,
                    });
                }
                let settled = PairingJournalEntry {
                    revision: revoking.revision + 1,
                    state: JournalHostState::RevocationSettled {
                        host_credential_status: status,
                    },
                    ..revoking.clone()
                };
                self.cas_journal(Some(&revoking), settled.clone()).await?;
                (settled, status)
            }
        };

        if !self.delete_entry_credentials(&settled).await {
            // The host result is already durable. Cleanup can retry without
            // needing the credential or reissuing the remote mutation.
            return Ok(RemoteRevokeOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }
        let revoked = PairingJournalEntry {
            revision: settled.revision + 1,
            credential_alias: String::new(),
            pending_credential_alias: None,
            operation_id: None,
            state: JournalHostState::Revoked,
            ..settled.clone()
        };
        self.cas_journal(Some(&settled), revoked).await?;
        Ok(RemoteRevokeOutcome::Revoked {
            host_id,
            host_credential_status: status,
        })
    }

    pub(crate) async fn forget(
        &self,
        host_id: RemoteHostId,
    ) -> Result<RemoteForgetOutcome, RemoteHostPairingError> {
        let host_lock = self.host_lock(&host_id).await;
        let _operation = host_lock.lock().await;
        self.invalidate_offers(&host_id).await;
        let current = match self.load_journal(&host_id).await? {
            Some(entry) => entry,
            None => return Ok(RemoteForgetOutcome::AlreadyForgotten { host_id }),
        };
        let (forgetting, host_revocation_still_required) = match current.state {
            JournalHostState::Forgotten => {
                return Ok(RemoteForgetOutcome::AlreadyForgotten { host_id });
            }
            JournalHostState::Forgetting {
                host_revocation_still_required,
            } => (current, host_revocation_still_required),
            _ => {
                // A local-only forget cannot prove any remote credential was
                // invalidated. For legacy v1 this means rotating the host-wide
                // bearer out-of-band; for v2 it means revoking the device grant.
                let host_revocation_still_required = !matches!(
                    current.state,
                    JournalHostState::EnrollmentRolledBack(_)
                        | JournalHostState::Revoked
                        | JournalHostState::RevocationSettled {
                            host_credential_status: HostCredentialRevocationStatus::Confirmed,
                        }
                        | JournalHostState::RePairRequired(RemoteRePairReason::CredentialRejected)
                        | JournalHostState::RePairRequired(RemoteRePairReason::Revoked)
                );
                let forgetting = PairingJournalEntry {
                    revision: current.revision + 1,
                    operation_id: None,
                    state: JournalHostState::Forgetting {
                        host_revocation_still_required,
                    },
                    ..current.clone()
                };
                self.cas_journal(Some(&current), forgetting.clone()).await?;
                (forgetting, host_revocation_still_required)
            }
        };
        self.host.close_local(&host_id).await;
        if !self.delete_entry_credentials(&forgetting).await {
            return Ok(RemoteForgetOutcome::NeedsRepair {
                host_id,
                reason: RemotePairingRepairReason::SecureStorageUnavailable,
            });
        }
        let finalized = PairingJournalEntry {
            revision: forgetting.revision + 1,
            credential_alias: String::new(),
            pending_credential_alias: None,
            state: JournalHostState::Forgotten,
            ..forgetting.clone()
        };
        self.cas_journal(Some(&forgetting), finalized).await?;
        Ok(RemoteForgetOutcome::ForgottenLocally {
            host_id,
            host_revocation_still_required,
        })
    }

    async fn idempotent_pair_result(
        &self,
        host_id: &RemoteHostId,
    ) -> Result<RemotePairingOutcome, RemoteHostPairingError> {
        match self.load_journal(host_id).await? {
            Some(entry) if entry.state == JournalHostState::Active => {
                Ok(RemotePairingOutcome::AlreadyPaired {
                    host_id: host_id.clone(),
                })
            }
            _ => Err(RemoteHostPairingError::UnknownOffer),
        }
    }

    async fn host_lock(&self, host_id: &RemoteHostId) -> Arc<Mutex<()>> {
        let mut locks = self.host_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(host_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(host_id.clone(), Arc::downgrade(&lock));
        lock
    }

    async fn load_journal(
        &self,
        host_id: &RemoteHostId,
    ) -> Result<Option<PairingJournalEntry>, RemoteHostPairingError> {
        self.journal
            .load(host_id)
            .await
            .map_err(|_| RemoteHostPairingError::JournalUnavailable)
    }

    async fn cas_journal(
        &self,
        previous: Option<&PairingJournalEntry>,
        replacement: PairingJournalEntry,
    ) -> Result<(), RemoteHostPairingError> {
        let host_id = replacement.host_id.clone();
        self.journal
            .compare_and_swap(&host_id, previous.map(|entry| entry.revision), replacement)
            .await
            .map_err(|error| match error {
                JournalError::Conflict | JournalError::Unavailable => {
                    RemoteHostPairingError::JournalUnavailable
                }
            })
    }

    async fn mark_needs_repair(
        &self,
        entry: PairingJournalEntry,
        reason: RemotePairingRepairReason,
    ) -> Result<(), RemoteHostPairingError> {
        let replacement = PairingJournalEntry {
            revision: entry.revision + 1,
            state: JournalHostState::NeedsRepair(reason),
            ..entry.clone()
        };
        self.cas_journal(Some(&entry), replacement).await
    }

    /// Persist compensation intent before calling the host. A restart replays
    /// rollback—not establish—with the original operation key.
    async fn rollback_pairing_transaction(
        &self,
        host_id: &RemoteHostId,
        operation_id: &str,
        pending: &PairingJournalEntry,
        reason: RemotePairingRepairReason,
    ) -> Result<bool, RemoteHostPairingError> {
        debug_assert_eq!(&pending.host_id, host_id);
        debug_assert_eq!(pending.operation_id.as_deref(), Some(operation_id));
        let rollback_pending = PairingJournalEntry {
            revision: pending.revision + 1,
            state: JournalHostState::EnrollmentRollbackPending(reason),
            ..pending.clone()
        };
        self.cas_journal(Some(pending), rollback_pending.clone())
            .await?;
        self.resume_pairing_rollback(rollback_pending).await
    }

    async fn resume_pairing_rollback(
        &self,
        rollback_pending: PairingJournalEntry,
    ) -> Result<bool, RemoteHostPairingError> {
        let reason = match rollback_pending.state {
            JournalHostState::EnrollmentRollbackPending(reason) => reason,
            _ => return Err(RemoteHostPairingError::JournalUnavailable),
        };
        let operation_id = rollback_pending
            .operation_id
            .as_deref()
            .ok_or(RemoteHostPairingError::JournalUnavailable)?;
        if self
            .host
            .rollback_establish(&rollback_pending.host_id, operation_id)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let rolled_back = PairingJournalEntry {
            revision: rollback_pending.revision + 1,
            operation_id: None,
            state: JournalHostState::EnrollmentRolledBack(reason),
            ..rollback_pending.clone()
        };
        self.cas_journal(Some(&rollback_pending), rolled_back)
            .await?;
        self.invalidate_offers(&rollback_pending.host_id).await;
        Ok(true)
    }

    /// Durably block reconnect before either removing a credential the host has
    /// authoritatively rejected or quarantining authority whose status is
    /// uncertain. Cleanup is idempotent; a failed delete leaves the tombstone
    /// and aliases in place for a later retry.
    async fn transition_to_repair_required(
        &self,
        entry: PairingJournalEntry,
        reason: RemoteRePairReason,
        disposition: CredentialDisposition,
    ) -> Result<bool, RemoteHostPairingError> {
        let tombstone = PairingJournalEntry {
            revision: entry.revision + 1,
            operation_id: None,
            state: JournalHostState::RePairRequired(reason),
            ..entry.clone()
        };
        self.cas_journal(Some(&entry), tombstone.clone()).await?;
        self.host.close_local(&entry.host_id).await;
        if matches!(disposition, CredentialDisposition::Quarantined) {
            return Ok(true);
        }
        if !self.delete_entry_credentials(&tombstone).await {
            return Ok(false);
        }
        let cleaned = PairingJournalEntry {
            revision: tombstone.revision + 1,
            credential_alias: String::new(),
            pending_credential_alias: None,
            ..tombstone.clone()
        };
        self.cas_journal(Some(&tombstone), cleaned).await?;
        Ok(true)
    }

    async fn delete_entry_credentials(&self, entry: &PairingJournalEntry) -> bool {
        if !entry.credential_alias.is_empty()
            && self.secrets.delete(&entry.credential_alias).await.is_err()
        {
            return false;
        }
        if let Some(alias) = entry.pending_credential_alias.as_deref()
            && !alias.is_empty()
            && alias != entry.credential_alias
            && self.secrets.delete(alias).await.is_err()
        {
            return false;
        }
        true
    }

    async fn read_credential(
        &self,
        entry: &PairingJournalEntry,
    ) -> Result<Option<ports::OpaqueCredential>, RemoteHostPairingError> {
        if entry.credential_alias.is_empty() {
            return Ok(None);
        }
        self.secrets
            .read(&entry.credential_alias)
            .await
            .map_err(|error| match error {
                SecretStoreError::Unavailable => RemoteHostPairingError::SecureStorageUnavailable,
            })
    }

    async fn consume_offer(&self, offer_id: &RemotePairingOfferId) {
        if let Some(cached) = self.offers.lock().await.get_mut(&offer_id.value) {
            cached.invite = None;
        }
    }

    async fn invalidate_offers(&self, host_id: &RemoteHostId) {
        self.offers
            .lock()
            .await
            .retain(|_, cached| &cached.public.host_id != host_id);
    }
}

fn normalize_runtime_offers(
    runtimes: Vec<RemoteRuntimeOffer>,
) -> Result<Vec<RemoteRuntimeOffer>, RemoteHostPairingError> {
    if runtimes.len() > MAX_RUNTIME_CHOICES {
        return Err(RemoteHostPairingError::ProtocolViolation);
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for runtime in runtimes {
        let runtime_id = runtime.runtime_id.trim();
        if runtime_id.is_empty()
            || runtime_id.chars().count() > MAX_RUNTIME_ID_CHARS
            || runtime_id.chars().any(char::is_control)
        {
            return Err(RemoteHostPairingError::ProtocolViolation);
        }
        if !seen.insert(runtime_id.to_string()) {
            continue;
        }
        let display_name = sanitize_display_name(&runtime.display_name);
        normalized.push(RemoteRuntimeOffer {
            runtime_id: runtime_id.to_string(),
            display_name: if display_name.is_empty() {
                runtime_id.to_string()
            } else {
                display_name
            },
            available: runtime.available,
            recommended: runtime.recommended && runtime.available,
        });
    }
    normalized.sort_by(|left, right| left.runtime_id.cmp(&right.runtime_id));
    Ok(normalized)
}

fn validate_runtime_selection(
    offered: &[RemoteRuntimeOffer],
    selected: Vec<String>,
) -> Result<Vec<String>, RemoteHostPairingError> {
    let available = offered
        .iter()
        .filter(|runtime| runtime.available)
        .map(|runtime| runtime.runtime_id.as_str())
        .collect::<HashSet<_>>();
    let mut normalized = selected
        .into_iter()
        .map(|runtime| runtime.trim().to_string())
        .filter(|runtime| !runtime.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty()
        || normalized.len() > MAX_RUNTIME_CHOICES
        || normalized
            .iter()
            .any(|runtime| !available.contains(runtime.as_str()))
    {
        return Err(RemoteHostPairingError::InvalidRuntimeSelection);
    }
    Ok(normalized)
}

fn valid_connected_runtime_set(desired: &[String], connected: &[String]) -> bool {
    let desired = desired.iter().map(String::as_str).collect::<HashSet<_>>();
    let connected = connected
        .iter()
        .map(|runtime| runtime.trim())
        .filter(|runtime| !runtime.is_empty())
        .collect::<HashSet<_>>();
    !connected.is_empty() && connected.iter().all(|runtime| desired.contains(runtime))
}

fn entry_has_credentials(entry: &PairingJournalEntry) -> bool {
    !entry.credential_alias.is_empty()
        || entry
            .pending_credential_alias
            .as_deref()
            .is_some_and(|alias| !alias.is_empty())
}

fn reconnect_success(host_id: RemoteHostId, already_connected: bool) -> RemoteReconnectOutcome {
    if already_connected {
        RemoteReconnectOutcome::AlreadyConnected { host_id }
    } else {
        RemoteReconnectOutcome::Connected { host_id }
    }
}

fn map_secret_error(_error: SecretStoreError) -> RemoteHostPairingError {
    RemoteHostPairingError::SecureStorageUnavailable
}

fn normalize_display_name(candidate: Option<String>, fallback: &str) -> String {
    let candidate = candidate
        .map(|value| sanitize_display_name(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| sanitize_display_name(fallback));
    if candidate.is_empty() {
        "Remote Host".to_string()
    } else {
        candidate
    }
}

fn sanitize_display_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_DISPLAY_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn map_host_error(error: HostPortError) -> RemoteHostPairingError {
    match error {
        HostPortError::AuthenticationRejected => RemoteHostPairingError::AuthenticationRejected,
        HostPortError::HostIdentityChanged => RemoteHostPairingError::HostIdentityChanged,
        HostPortError::CredentialRevoked => RemoteHostPairingError::CredentialRevoked,
        HostPortError::Unavailable => RemoteHostPairingError::HostUnavailable,
        HostPortError::ProtocolViolation => RemoteHostPairingError::ProtocolViolation,
        HostPortError::V2Unavailable => RemoteHostPairingError::V2HostProtocolUnavailable,
        HostPortError::Cancelled => RemoteHostPairingError::Cancelled,
    }
}

#[cfg(test)]
mod tests;
