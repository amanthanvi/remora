//! Transitional UniFFI registration for the secret-free code classifier.
//!
//! The full pair/reconnect/revoke/forget lifecycle stays in the deep Rust
//! module until the Remora Link v2 host adapter and platform secret adapter are
//! wired. Exporting only the honest local operation prevents callers from
//! mistaking an unsupported adapter for network success.

use crate::remote_host_pairing::types::{
    RemoteHostPairingError, RemotePairingCode, RemotePairingCodeInspection,
};

#[derive(uniffi::Object)]
pub struct RemoteHostPairingInspector;

#[uniffi::export]
impl RemoteHostPairingInspector {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self
    }

    /// Strictly classify raw JSON or the canonical
    /// `remora-link:v2:<base64url-json>` copy/paste envelope.
    ///
    /// This never performs network I/O and never returns invitation material.
    pub fn inspect_pairing_code(
        &self,
        code: RemotePairingCode,
    ) -> Result<RemotePairingCodeInspection, RemoteHostPairingError> {
        crate::remote_host_pairing::inspect_remote_pairing_code(code)
    }
}
