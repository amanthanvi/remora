//! Narrow internal seams owned by the pairing module.
//!
//! Adapters depend inward on these semantic contracts. The core never deals in
//! QUIC streams, JSON frames, relay URLs, Keychain status codes, or platform
//! preference records.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use zeroize::Zeroize;

use super::identity::DecodedPairingCode;
use super::types::{
    HostCredentialRevocationStatus, RemoteHostId, RemotePairingProtocol, RemotePairingRepairReason,
    RemoteRuntimeOffer,
};

#[derive(Clone)]
pub(crate) struct HostInspectRequest {
    pub(crate) invite: DecodedPairingCode,
}

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedHostOffer {
    pub(crate) host_id: RemoteHostId,
    pub(crate) suggested_display_name: String,
    pub(crate) runtimes: Vec<RemoteRuntimeOffer>,
}

#[derive(Clone)]
pub(crate) struct HostEstablishRequest {
    pub(crate) invite: DecodedPairingCode,
    pub(crate) display_name: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) idempotency_key: String,
}

impl fmt::Debug for HostEstablishRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostEstablishRequest")
            .field("invite", &self.invite)
            .field("display_name", &self.display_name)
            .field("selected_runtime_ids", &self.selected_runtime_ids)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct HostReconnectRequest {
    pub(crate) host_id: RemoteHostId,
    pub(crate) credential: OpaqueCredential,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) idempotency_key: String,
}

#[derive(Clone)]
pub(crate) struct HostConfirmReconnectRequest {
    pub(crate) host_id: RemoteHostId,
    pub(crate) credential: OpaqueCredential,
    pub(crate) idempotency_key: String,
}

impl fmt::Debug for HostConfirmReconnectRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostConfirmReconnectRequest")
            .field("host_id", &self.host_id)
            .field("credential", &"<redacted>")
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

impl fmt::Debug for HostReconnectRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostReconnectRequest")
            .field("host_id", &self.host_id)
            .field("credential", &"<redacted>")
            .field("selected_runtime_ids", &self.selected_runtime_ids)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct HostRevokeRequest {
    pub(crate) host_id: RemoteHostId,
    pub(crate) credential: OpaqueCredential,
    pub(crate) idempotency_key: String,
}

impl fmt::Debug for HostRevokeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRevokeRequest")
            .field("host_id", &self.host_id)
            .field("credential", &"<redacted>")
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

/// Credential bytes returned by the host adapter. Debug output is always
/// redacted and the allocation is cleared when its final clone is dropped.
#[derive(Clone)]
pub(crate) struct OpaqueCredential(Arc<CredentialBytes>);

struct CredentialBytes(Vec<u8>);

impl Drop for CredentialBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl OpaqueCredential {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(CredentialBytes(bytes)))
    }

    pub(crate) fn expose_for_adapter(&self) -> &[u8] {
        &self.0.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }
}

impl fmt::Debug for OpaqueCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueCredential(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EstablishedRemoteHost {
    /// The adapter may return `already_connected` only after proving that the
    /// existing session is the same pinned host and satisfies the requested
    /// runtime set.
    pub(crate) already_connected: bool,
    /// At least one runtime must be present before the core can commit.
    pub(crate) connected_runtime_ids: Vec<String>,
    /// Pairing must return a fresh device-bound grant. Reconnect may return a
    /// staged replacement credential; the supplied credential must remain
    /// valid until `confirm_reconnect` succeeds. `None` means the stored grant
    /// remains authoritative. Credentials always stay behind the secret port.
    pub(crate) credential: Option<OpaqueCredential>,
}

/// AuthenticationRejected and CredentialRevoked definitively mean every
/// credential associated with the operation is unusable. HostIdentityChanged
/// and V2Unavailable require re-pairing but do not prove that the old grant was
/// revoked, so the core quarantines its credential for revoke/forget. Unavailable
/// and Cancelled are ambiguous because a success response may have been lost
/// after host commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostPortError {
    AuthenticationRejected,
    HostIdentityChanged,
    CredentialRevoked,
    Unavailable,
    ProtocolViolation,
    V2Unavailable,
    Cancelled,
}

#[async_trait]
pub(crate) trait RemotePairingHostPort: Send + Sync {
    /// Authenticates the host before returning metadata. The Remora Link v2
    /// adapter must verify P-256/SHA-256 over the canonical transcript bytes in
    /// message mode exactly once; an endpoint identity alone is not proof and
    /// callers must not prehash the transcript before the signing API.
    async fn inspect(
        &self,
        request: HostInspectRequest,
    ) -> Result<AuthenticatedHostOffer, HostPortError>;

    /// Establishes authenticated runtime resources and returns a device-bound
    /// credential. Implementations must be idempotent for the supplied key and
    /// must not report success until at least one requested runtime is attached.
    async fn establish(
        &self,
        request: HostEstablishRequest,
    ) -> Result<EstablishedRemoteHost, HostPortError>;

    async fn reconnect(
        &self,
        request: HostReconnectRequest,
    ) -> Result<EstablishedRemoteHost, HostPortError>;

    /// Commits a staged reconnect credential and invalidates its predecessor.
    /// Both reconnect phases must be durably idempotent for the same key:
    /// replaying `reconnect` returns the same staged credential, and replaying
    /// confirmation with that credential returns the same terminal result.
    /// Until confirmation succeeds, the original credential remains valid and
    /// either credential can revoke the complete device grant.
    async fn confirm_reconnect(
        &self,
        request: HostConfirmReconnectRequest,
    ) -> Result<(), HostPortError>;

    async fn revoke(
        &self,
        request: HostRevokeRequest,
    ) -> Result<HostCredentialRevocationStatus, HostPortError>;

    /// Compensating action for an enrollment whose local commit failed. `Ok`
    /// means the adapter authoritatively confirmed no grant remains; any error
    /// leaves the original pairing transaction pending and retryable.
    async fn rollback_establish(
        &self,
        host_id: &RemoteHostId,
        idempotency_key: &str,
    ) -> Result<(), HostPortError>;

    /// Immediately removes local live resources. It does not claim remote
    /// credential revocation.
    async fn close_local(&self, host_id: &RemoteHostId);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalHostState {
    CommitPending,
    /// A reconnect intent and idempotency key are durable, but no replacement
    /// credential has been durably staged locally yet.
    ReconnectPending,
    /// A staged replacement credential is durable. Host confirmation must be
    /// replayed with the same operation key before the old credential is removed.
    ReconnectConfirmPending {
        already_connected: bool,
    },
    /// Host confirmation is durable remotely and in the journal. Only local
    /// cleanup and alias promotion remain.
    ReconnectSettled {
        already_connected: bool,
    },
    /// Enrollment compensation is durable and must be replayed instead of
    /// re-establishing the grant.
    EnrollmentRollbackPending(RemotePairingRepairReason),
    /// Enrollment compensation authoritatively proved that no remote grant
    /// remains. Only cleanup of a possibly partial local secret write remains.
    EnrollmentRolledBack(RemotePairingRepairReason),
    Active,
    Revoking,
    /// Remote revocation reached an authoritative terminal result. Only local
    /// secret cleanup remains, which is safe to retry without the credential.
    RevocationSettled {
        host_credential_status: HostCredentialRevocationStatus,
    },
    Revoked,
    /// Local deletion was committed before touching secret storage. The flag
    /// preserves whether the host still needs an explicit device-grant revoke
    /// across cleanup retries.
    Forgetting {
        host_revocation_still_required: bool,
    },
    Forgotten,
    RePairRequired(super::types::RemoteRePairReason),
    NeedsRepair(RemotePairingRepairReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PairingJournalEntry {
    pub(crate) revision: u64,
    pub(crate) generation: u64,
    pub(crate) host_id: RemoteHostId,
    pub(crate) protocol: RemotePairingProtocol,
    pub(crate) display_name: String,
    pub(crate) desired_runtime_ids: Vec<String>,
    pub(crate) credential_alias: String,
    /// Secret-store alias for a staged reconnect credential. The journal never
    /// contains credential bytes.
    pub(crate) pending_credential_alias: Option<String>,
    /// Non-secret idempotency key for an interrupted host mutation. Pair and
    /// reconnect/revoke retries reuse this value across cancellation or process
    /// restart.
    pub(crate) operation_id: Option<String>,
    pub(crate) state: JournalHostState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalError {
    Conflict,
    Unavailable,
}

#[async_trait]
pub(crate) trait PairingJournalPort: Send + Sync {
    async fn load(
        &self,
        host_id: &RemoteHostId,
    ) -> Result<Option<PairingJournalEntry>, JournalError>;

    /// Compare-and-swap one complete, non-secret host record. `expected_revision`
    /// is `None` only for a host with no record.
    async fn compare_and_swap(
        &self,
        host_id: &RemoteHostId,
        expected_revision: Option<u64>,
        replacement: PairingJournalEntry,
    ) -> Result<(), JournalError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretStoreError {
    Unavailable,
}

#[async_trait]
pub(crate) trait PairingSecretPort: Send + Sync {
    async fn read(&self, alias: &str) -> Result<Option<OpaqueCredential>, SecretStoreError>;
    /// Atomically replaces the value for one alias. An error must leave the
    /// prior value intact or absent; partial writes are not observable.
    async fn write(
        &self,
        alias: &str,
        credential: OpaqueCredential,
    ) -> Result<(), SecretStoreError>;
    async fn delete(&self, alias: &str) -> Result<(), SecretStoreError>;
}

pub(crate) trait PairingClock: Send + Sync {
    fn unix_seconds(&self) -> u64;
}

pub(crate) trait PairingIdSource: Send + Sync {
    fn next_id(&self, purpose: &'static str) -> String;
}

pub(crate) struct SystemClock;

impl PairingClock for SystemClock {
    fn unix_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

pub(crate) struct RandomIdSource;

impl PairingIdSource for RandomIdSource {
    fn next_id(&self, purpose: &'static str) -> String {
        format!("{purpose}_{}", uuid::Uuid::new_v4().simple())
    }
}
