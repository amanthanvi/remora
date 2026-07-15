//! Display-safe boundary types for remote-host pairing.
//!
//! These types describe user intent and lifecycle outcomes. Transport
//! credentials, relay addresses, endpoint identities, device keys, grants,
//! sequence cursors, and wire-format details deliberately do not appear here.

/// Exact text captured from the host's QR code or copy/paste output.
///
/// This value is sensitive input. It must never be logged or embedded in an
/// observable snapshot.
#[derive(Clone, uniffi::Record)]
pub struct RemotePairingCode {
    pub encoded: String,
}

/// Stable paired-host identity, derived and validated in Rust.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct RemoteHostId {
    pub value: String,
}

/// Process-local capability identifying one inspected offer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct RemotePairingOfferId {
    pub value: String,
}

/// Pairing protocol generation hidden behind the semantic module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemotePairingProtocol {
    /// Existing `alleycat/1` host-wide bearer-token protocol. This remains
    /// recognizable for compatibility, but cannot issue a v2 device grant.
    LegacyV1,
    /// Remora Link device-bound grant protocol.
    DeviceGrantV2,
}

/// Whether an inspected code may enter the new pairing transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemotePairingOfferDisposition {
    Ready,
    /// The code belongs to the legacy bearer protocol. The caller must obtain
    /// a fresh v2 offer; the module never upgrades a v1 token into a grant.
    RePairRequired,
}

/// Secret-free result of locally classifying a pairing code.
///
/// This is useful before a network adapter is available. It is intentionally
/// not named an authenticated host offer: runtime metadata is added only after
/// the host port verifies the same identity.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RemotePairingCodeInspection {
    pub host_id: RemoteHostId,
    pub suggested_display_name: String,
    pub protocol: RemotePairingProtocol,
    pub disposition: RemotePairingOfferDisposition,
    pub expires_at_unix_ms: Option<u64>,
}

/// Semantic runtime advertised by an authenticated host offer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RemoteRuntimeOffer {
    pub runtime_id: String,
    pub display_name: String,
    pub available: bool,
    pub recommended: bool,
}

/// Secret-free, short-lived offer returned after host inspection.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RemotePairingOffer {
    pub offer_id: RemotePairingOfferId,
    pub host_id: RemoteHostId,
    pub suggested_display_name: String,
    pub protocol: RemotePairingProtocol,
    pub disposition: RemotePairingOfferDisposition,
    pub runtimes: Vec<RemoteRuntimeOffer>,
    pub expires_at_unix_ms: u64,
}

/// User acceptance of the exact offer revision held by Rust.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RemotePairingAcceptance {
    pub offer_id: RemotePairingOfferId,
    pub display_name: Option<String>,
    pub selected_runtime_ids: Vec<String>,
}

/// Why an operation can only continue after a fresh v2 pairing ceremony.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemoteRePairReason {
    LegacyBearerCredential,
    MissingPairing,
    OfferExpired,
    HostIdentityChanged,
    CredentialRejected,
    Revoked,
    V2HostProtocolUnavailable,
}

/// Why locally persisted state needs repair before it can be used safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemotePairingRepairReason {
    MissingHostCredential,
    InterruptedCommit,
    InterruptedReconnect,
    InterruptedRevocation,
    HostCredentialNeedsRevocation,
    UnsupportedStoredVersion,
    SecureStorageUnavailable,
    JournalUnavailable,
}

/// Result of accepting an inspected offer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemotePairingOutcome {
    Paired {
        host_id: RemoteHostId,
    },
    AlreadyPaired {
        host_id: RemoteHostId,
    },
    RePairRequired {
        host_id: RemoteHostId,
        reason: RemoteRePairReason,
    },
    NeedsRepair {
        host_id: RemoteHostId,
        reason: RemotePairingRepairReason,
    },
}

/// Result of connecting an already paired host by opaque ID only.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemoteReconnectOutcome {
    Connected {
        host_id: RemoteHostId,
    },
    AlreadyConnected {
        host_id: RemoteHostId,
    },
    TemporarilyUnavailable {
        host_id: RemoteHostId,
    },
    RePairRequired {
        host_id: RemoteHostId,
        reason: RemoteRePairReason,
    },
    NeedsRepair {
        host_id: RemoteHostId,
        reason: RemotePairingRepairReason,
    },
}

/// How strongly the host confirmed credential revocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostCredentialRevocationStatus {
    Confirmed,
    /// The durable local tombstone is active; host confirmation should be
    /// retried later.
    Deferred,
    /// Legacy v1 has no device-scoped remote revoke operation.
    UnsupportedByLegacyProtocol,
}

/// Result of host-authoritative revocation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemoteRevokeOutcome {
    Revoked {
        host_id: RemoteHostId,
        host_credential_status: HostCredentialRevocationStatus,
    },
    AlreadyRevoked {
        host_id: RemoteHostId,
    },
    NeedsRepair {
        host_id: RemoteHostId,
        reason: RemotePairingRepairReason,
    },
}

/// Result of deleting this device's local relationship with a host.
///
/// Forget is intentionally distinct from revoke: it never claims that the
/// host invalidated this device's grant.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemoteForgetOutcome {
    ForgottenLocally {
        host_id: RemoteHostId,
        host_revocation_still_required: bool,
    },
    AlreadyForgotten {
        host_id: RemoteHostId,
    },
    NeedsRepair {
        host_id: RemoteHostId,
        reason: RemotePairingRepairReason,
    },
}

/// Sanitized errors at the module boundary. No variant contains raw adapter
/// messages or pairing input.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RemoteHostPairingError {
    #[error("pairing code is malformed")]
    MalformedCode,
    #[error("pairing code exceeds the supported size")]
    CodeTooLarge,
    #[error("pairing code is missing a host identity")]
    MissingHostIdentity,
    #[error("pairing code has an invalid host identity")]
    InvalidHostIdentity,
    #[error("pairing code is missing enrollment material")]
    MissingEnrollmentMaterial,
    #[error("pairing code has invalid enrollment material")]
    InvalidEnrollmentMaterial,
    #[error("pairing code has an invalid relay hint")]
    InvalidRelayHint,
    #[error("manual short-code pairing is not supported securely yet")]
    UnsupportedManualCode,
    #[error("pairing protocol is not supported")]
    IncompatibleProtocol,
    #[error("pairing offer expired")]
    OfferExpired,
    #[error("pairing offer is unknown or already consumed")]
    UnknownOffer,
    #[error("selected runtimes are not valid for this offer")]
    InvalidRuntimeSelection,
    #[error("host rejected authentication")]
    AuthenticationRejected,
    #[error("remote host identity changed")]
    HostIdentityChanged,
    #[error("remote host credential was revoked")]
    CredentialRevoked,
    #[error("remote host is unavailable")]
    HostUnavailable,
    #[error("no selected remote runtime connected")]
    NoRuntimeConnected,
    #[error("Remora Link v2 host support is not available")]
    V2HostProtocolUnavailable,
    #[error("remote host violated the pairing protocol")]
    ProtocolViolation,
    #[error("pairing journal is unavailable")]
    JournalUnavailable,
    #[error("secure pairing storage is unavailable")]
    SecureStorageUnavailable,
    #[error("pairing operation was cancelled")]
    Cancelled,
}
