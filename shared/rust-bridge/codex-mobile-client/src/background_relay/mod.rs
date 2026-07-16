//! Rust-owned background relay lifecycle.
//!
//! The module deliberately presents a semantic interface rather than an HTTP
//! client. It owns per-host installation bindings, provider-token fan-out,
//! cursor ordering, authoritative repair, and acknowledgement ordering. Native
//! code remains responsible only for observing APNs/FCM tokens, validating the
//! provider carrier envelope, and supplying an opaque secure-storage adapter.

use std::sync::Arc;

mod coordinator;
mod http;
mod ports;
mod types;

pub(crate) use coordinator::BackgroundRelay;
pub(crate) use http::ReqwestRelayTransport;
pub(crate) use ports::*;
pub(crate) use types::*;

/// Process-local relay configuration retained by `MobileClient` so every
/// `AppClient` handle observes the same adapters and lifecycle.
pub(crate) struct ConfiguredBackgroundRelay {
    pub(crate) relay: Arc<BackgroundRelay>,
    pub(crate) allow_loopback_http: bool,
}

#[cfg(test)]
mod tests;
