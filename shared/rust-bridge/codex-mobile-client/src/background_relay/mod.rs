//! Rust-owned background relay lifecycle.
//!
//! The module deliberately presents a semantic interface rather than an HTTP
//! client. It owns per-host installation bindings, provider-token fan-out,
//! cursor ordering, authoritative repair, and acknowledgement ordering. Native
//! code remains responsible only for observing APNs/FCM tokens, validating the
//! provider carrier envelope, and supplying an opaque secure-storage adapter.

mod coordinator;
mod ports;
mod types;

pub(crate) use coordinator::BackgroundRelay;
pub(crate) use ports::*;
pub(crate) use types::*;

#[cfg(test)]
mod tests;
