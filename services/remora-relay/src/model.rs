use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{RelayError, Result, SCHEMA_VERSION};

pub const MAX_OPAQUE_ID_LEN: usize = 128;
pub const MIN_OPAQUE_ID_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueId(String);

impl OpaqueId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() < MIN_OPAQUE_ID_LEN
            || value.len() > MAX_OPAQUE_ID_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(RelayError::Invalid("opaque identifier"));
        }
        Ok(Self(value))
    }

    pub fn random(prefix: &str) -> Self {
        debug_assert!(prefix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        Self(format!("{prefix}_{}", uuid::Uuid::new_v4().simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpaqueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueId([redacted])")
    }
}

impl fmt::Display for OpaqueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl FromStr for OpaqueId {
    type Err = RelayError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

#[derive(Clone)]
pub struct PresentedCapability(SecretString);

impl PresentedCapability {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() < 32 || value.len() > 256 || value.chars().any(char::is_whitespace) {
            return Err(RelayError::Unauthorized);
        }
        Ok(Self(SecretString::from(value)))
    }

    pub(crate) fn expose(&self) -> &str {
        use secrecy::ExposeSecret as _;
        self.0.expose_secret()
    }
}

impl fmt::Debug for PresentedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PresentedCapability([redacted])")
    }
}

#[derive(Clone)]
pub struct IssuedCapability(Zeroizing<String>);

impl IssuedCapability {
    pub(crate) fn random() -> Self {
        let mut bytes = [0_u8; 32];
        rand::fill(&mut bytes);
        Self(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IssuedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IssuedCapability([redacted])")
    }
}

impl Serialize for IssuedCapability {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Write,
    Read,
    Manage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    StateChanged,
    ActivityChanged,
    ConnectionChanged,
    SecurityChanged,
}

impl EventClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StateChanged => "state_changed",
            Self::ActivityChanged => "activity_changed",
            Self::ConnectionChanged => "connection_changed",
            Self::SecurityChanged => "security_changed",
        }
    }
}

impl FromStr for EventClass {
    type Err = RelayError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "state_changed" => Ok(Self::StateChanged),
            "activity_changed" => Ok(Self::ActivityChanged),
            "connection_changed" => Ok(Self::ConnectionChanged),
            "security_changed" => Ok(Self::SecurityChanged),
            _ => Err(RelayError::Invalid("event class")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushProviderKind {
    Apns,
    Fcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushEnvironment {
    Sandbox,
    Production,
}

impl PushEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Production => "production",
        }
    }
}

impl FromStr for PushEnvironment {
    type Err = RelayError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "sandbox" => Ok(Self::Sandbox),
            "production" => Ok(Self::Production),
            _ => Err(RelayError::Invalid("push environment")),
        }
    }
}

impl PushProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }
}

impl FromStr for PushProviderKind {
    type Err = RelayError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "apns" => Ok(Self::Apns),
            "fcm" => Ok(Self::Fcm),
            _ => Err(RelayError::Invalid("push provider")),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestEventRequest {
    pub event_id: OpaqueId,
    pub event_class: EventClass,
    pub expires_at_ms: i64,
    /// URL-safe, unpadded base64 of end-to-end encrypted bytes.
    pub ciphertext: String,
    #[serde(default)]
    pub snapshot: Option<SnapshotUpdate>,
}

impl fmt::Debug for IngestEventRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestEventRequest")
            .field("event_id", &self.event_id)
            .field("event_class", &self.event_class)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("ciphertext", &"[redacted]")
            .field("snapshot", &self.snapshot.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotUpdate {
    pub revision: u64,
    pub expires_at_ms: i64,
    /// URL-safe, unpadded base64 of an end-to-end encrypted complete snapshot.
    pub ciphertext: String,
}

impl fmt::Debug for SnapshotUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotUpdate")
            .field("revision", &self.revision)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("ciphertext", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IngestEventResponse {
    pub schema_version: u16,
    pub event_id: OpaqueId,
    pub cursor: u64,
    pub replayed: bool,
}

impl IngestEventResponse {
    pub(crate) fn new(event_id: OpaqueId, cursor: u64, replayed: bool) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            event_id,
            cursor,
            replayed,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct IssuedInstallation {
    pub schema_version: u16,
    pub installation_id: OpaqueId,
    pub write_capability: IssuedCapability,
    pub read_capability: IssuedCapability,
    pub manage_capability: IssuedCapability,
}

impl fmt::Debug for IssuedInstallation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedInstallation")
            .field("installation_id", &self.installation_id)
            .field("write_capability", &"[redacted]")
            .field("read_capability", &"[redacted]")
            .field("manage_capability", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EventEnvelope {
    pub event_id: OpaqueId,
    pub cursor: u64,
    pub event_class: EventClass,
    pub expires_at_ms: i64,
    pub ciphertext: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EventPage {
    pub schema_version: u16,
    pub requested_after: u64,
    pub next_cursor: u64,
    pub high_watermark: u64,
    pub replay_floor: u64,
    pub reset_required: bool,
    pub snapshot_available: bool,
    pub events: Vec<EventEnvelope>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SnapshotEnvelope {
    pub schema_version: u16,
    pub revision: u64,
    pub through_cursor: u64,
    pub expires_at_ms: i64,
    pub ciphertext: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterDeviceRequest {
    pub provider: PushProviderKind,
    pub environment: PushEnvironment,
    pub token: String,
}

impl fmt::Debug for RegisterDeviceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterDeviceRequest")
            .field("provider", &self.provider)
            .field("environment", &self.environment)
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeviceRegistrationResponse {
    pub schema_version: u16,
    pub installation_id: OpaqueId,
    pub registration_id: OpaqueId,
    pub provider: PushProviderKind,
    pub environment: PushEnvironment,
    pub generation: u64,
    pub replaced: bool,
    #[serde(skip)]
    pub(crate) created: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TombstoneRegistrationRequest {
    pub through_generation: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OpaqueWakeHint {
    pub schema_version: u16,
    pub installation_id: OpaqueId,
    pub event_id: OpaqueId,
    pub cursor: u64,
    pub event_class: EventClass,
    pub expires_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_reject_semantic_or_unbounded_values() {
        assert!(OpaqueId::parse("short").is_err());
        assert!(OpaqueId::parse("this contains spaces and meaning").is_err());
        assert!(OpaqueId::parse("a".repeat(MAX_OPAQUE_ID_LEN + 1)).is_err());
        assert!(OpaqueId::parse("evt_0123456789abcdef").is_ok());
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let capability = PresentedCapability::parse("x".repeat(32)).unwrap();
        assert!(!format!("{capability:?}").contains(&"x".repeat(32)));
        let request = RegisterDeviceRequest {
            provider: PushProviderKind::Apns,
            environment: PushEnvironment::Sandbox,
            token: "secret-push-token".into(),
        };
        assert!(!format!("{request:?}").contains("secret-push-token"));
    }
}
