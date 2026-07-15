//! Durable, ciphertext-blind event delivery for Remora.
//!
//! The relay persists authoritative cursors and encrypted application payloads.
//! Push providers receive only typed, expiring wake hints. Provider delivery is
//! never interpreted as application acknowledgement or canonical state.

pub mod api;
pub mod backend;
pub mod config;
pub mod crypto;
pub mod error;
pub mod metrics;
pub mod model;
pub mod push;
pub mod store;
pub mod worker;

pub use api::{ApiState, build_router};
pub use backend::RelayBackend;
pub use config::{BootstrapAuth, DeploymentProfile, RelayConfig};
pub use crypto::TokenCipher;
pub use error::{RelayError, Result};
pub use metrics::RelayMetrics;
pub use model::*;
pub use push::*;
pub use store::{
    DeliveryOutcome, FaultInjector, FaultPoint, MaintenanceResult, NoFaults, OutboxLease,
    PostgresRelayStore, RelayStore, StoreDiagnostics, StoreLimits,
};
pub use worker::PushDispatcher;

/// Current HTTP and provider wake schema.
pub const SCHEMA_VERSION: u16 = 1;
