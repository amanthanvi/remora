//! Platform and transport seams for the Remora Link v2 lifecycle.
//!
//! The lifecycle owns protocol state, transcripts, replay policy, and journal
//! ordering. Adapters only provide a completed authenticated transport, a
//! non-exportable signing key, entropy, and durable nonsecret storage.

use async_trait::async_trait;

use super::wire::{ProofV2, RequestV2, ResponseV2};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HardwareKeyV2 {
    /// Stable platform reference (Keychain tag / Android Keystore alias).
    /// This is a locator, never private-key material.
    pub(crate) slot: String,
    /// Uncompressed SEC1 P-256 public key, base64url without padding.
    pub(crate) public_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CredentialPortError {
    #[error("hardware-backed credential storage is unavailable")]
    Unavailable,
    #[error("hardware-backed credential is missing")]
    Missing,
    #[error("hardware-backed credential returned an invalid signature")]
    InvalidSignature,
}

/// Custody for one non-exportable P-256 key per Remora Link host.
///
/// `sign_message` must perform ECDSA-with-SHA-256 over `message` exactly once.
/// The caller supplies the canonical transcript bytes, not a prehash.
#[async_trait]
pub(crate) trait CredentialCustodyPortV2: Send + Sync {
    async fn ensure_hardware_key(
        &self,
        host_id: &str,
    ) -> Result<HardwareKeyV2, CredentialPortError>;

    async fn load_hardware_key(
        &self,
        slot: &str,
    ) -> Result<Option<HardwareKeyV2>, CredentialPortError>;

    async fn sign_message(
        &self,
        slot: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, CredentialPortError>;

    /// Idempotent deletion. `Missing` is treated as success by lifecycle
    /// cleanup because a prior crash may have happened after deletion.
    async fn delete_hardware_key(&self, slot: &str) -> Result<(), CredentialPortError>;
}

/// Local cryptographic randomness and stable operation identities.
pub(crate) trait EntropyPortV2: Send + Sync {
    fn fresh_nonce(&self) -> [u8; 32];
    fn fresh_idempotency_key(&self, purpose: &'static str) -> String;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostRouteV2 {
    /// Pinned Iroh endpoint identity from the v2 invitation.
    pub(crate) node_id: String,
    /// Optional routing hint only. It is never used as identity or authority.
    pub(crate) relay_hint: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StartedExchangeV2 {
    /// Opaque, process-local transport handle. It is deliberately never
    /// journaled; recovery starts a fresh proof exchange.
    pub(crate) exchange_id: String,
    pub(crate) authenticated_host_endpoint_id: String,
    pub(crate) authenticated_client_endpoint_id: String,
    pub(crate) challenge_response: ResponseV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinishedExchangeV2 {
    pub(crate) response: ResponseV2,
    /// Exact process-local custody identity for a successfully attached
    /// runtime stream. Non-connect exchanges never produce an attachment.
    pub(crate) attachment_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HostPortErrorV2 {
    #[error("Remora Link host is temporarily unavailable")]
    Unavailable,
    #[error("Remora Link transport violated the control contract")]
    ProtocolViolation,
}

/// Two-phase host exchange preserving the request/challenge/proof ordering.
///
/// `start_exchange` must return endpoint IDs from the completed authenticated
/// transport. It must not accept replayable early data. `finish_exchange`
/// writes the proof on that same control stream and returns the terminal
/// response; for `connect`, the adapter retains the now-attached byte stream.
#[async_trait]
pub(crate) trait HostPortV2: Send + Sync {
    async fn start_exchange(
        &self,
        route: &HostRouteV2,
        request: &RequestV2,
    ) -> Result<StartedExchangeV2, HostPortErrorV2>;

    async fn finish_exchange(
        &self,
        exchange_id: &str,
        proof: &ProofV2,
    ) -> Result<FinishedExchangeV2, HostPortErrorV2>;

    /// Best-effort disposal for an exchange that cannot safely send a proof.
    /// Remove and close a retained exchange using only bounded local work.
    /// This must be synchronous and idempotent so a cancellation guard can
    /// invoke it safely from `Drop`, including on a foreign executor thread.
    fn abandon_exchange(&self, exchange_id: &str);

    /// Close locally retained runtime streams for one host. This performs no
    /// remote authorization mutation and is therefore used by both revoke and
    /// local-forget cleanup.
    async fn close_local(&self, host_id: &str);
}
