use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::{EndpointId, RelayUrl};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use super::remora_link_v2::{ConfirmationModeV2, DeviceScopeV2, validate_policy};
pub(crate) const REMORA_LINK_V2_ALPN: &[u8] = b"remora-link/2";
const LEGACY_V1_PROTOCOL_VERSION: u32 = 1;
const REMORA_LINK_V2_ENVELOPE_PREFIX: &str = "remora-link:v2:";
const MAX_ENCODED_SEGMENT_BYTES: usize = 4096;
const MAX_DECODED_JSON_BYTES: usize = 4096;
const V2_INVITATION_ID_BYTES: usize = 16;
const V2_SECRET_BYTES: usize = 32;
const MAX_HOST_NAME_BYTES: usize = 255;
const MAX_ENDPOINT_ID_BYTES: usize = 256;

/// Private, credential-bearing result of decoding a code. Its custom Debug
/// implementation is intentionally redacted.
#[derive(Clone)]
pub(crate) enum DecodedPairingCode {
    LegacyV1 { params: LegacyV1Invitation },
    DeviceGrantV2(V2Invite),
}

impl DecodedPairingCode {
    pub(crate) fn suggested_display_name(&self) -> String {
        match self {
            Self::LegacyV1 { params } => params
                .host_name
                .as_deref()
                .map(sanitize_host_name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Remote Host".to_string()),
            Self::DeviceGrantV2(invite) => invite
                .host_name
                .clone()
                .unwrap_or_else(|| "Remora Link".to_string()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct LegacyV1Invitation {
    pub(crate) node_id: String,
    host_name: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PairingCodeError {
    MalformedCode,
    CodeTooLarge,
    IncompatibleProtocol,
    UnsupportedManualCode,
    InvalidEnrollmentMaterial,
    MissingHostIdentity,
    InvalidHostIdentity,
    MissingEnrollmentMaterial,
    InvalidRelayHint,
}

impl fmt::Debug for DecodedPairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyV1 { params } => formatter
                .debug_struct("LegacyV1")
                .field("host_id", &format_args!("alleycat:{}", params.node_id))
                .field("credential", &"<redacted>")
                .finish(),
            Self::DeviceGrantV2(invite) => formatter
                .debug_struct("DeviceGrantV2")
                .field("host_id", &format_args!("remora-link:{}", invite.node_id))
                .field("invitation_id", &"<redacted>")
                .field("secret", &"<redacted>")
                .field("expires_at", &invite.expires_at)
                .finish(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct V2Invite {
    pub(crate) node_id: String,
    pub(crate) invitation_id: Vec<u8>,
    pub(crate) secret: Vec<u8>,
    pub(crate) expires_at: i64,
    pub(crate) max_runtime_ids: Vec<String>,
    pub(crate) max_scopes: Vec<DeviceScopeV2>,
    pub(crate) confirmation_mode: ConfirmationModeV2,
    pub(crate) host_name: Option<String>,
    pub(crate) relay: Option<String>,
}

impl Drop for V2Invite {
    fn drop(&mut self) {
        self.invitation_id.zeroize();
        self.secret.zeroize();
    }
}

impl fmt::Debug for V2Invite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("V2Invite")
            .field("node_id", &self.node_id)
            .field("invitation_id", &"<redacted>")
            .field("secret", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("host_name", &self.host_name)
            .field("relay", &self.relay.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    v: u32,
}

#[derive(Deserialize)]
struct LegacyV1InvitationWire {
    v: u32,
    node_id: String,
    token: String,
    relay: Option<String>,
    #[serde(default, alias = "hostname", alias = "display_name", alias = "name")]
    host_name: Option<String>,
}

impl Drop for LegacyV1InvitationWire {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V2InviteWire {
    v: u32,
    node_id: String,
    invitation_id: String,
    secret: String,
    expires_at: i64,
    max_runtime_ids: Vec<String>,
    max_scopes: Vec<DeviceScopeV2>,
    confirmation_mode: ConfirmationModeV2,
    #[serde(default)]
    host_name: Option<String>,
    #[serde(default)]
    relay: Option<String>,
}

impl Drop for V2InviteWire {
    fn drop(&mut self) {
        self.invitation_id.zeroize();
        self.secret.zeroize();
    }
}

pub(crate) fn decode_pairing_code(
    encoded: String,
    now_unix_seconds: u64,
) -> Result<DecodedPairingCode, PairingCodeError> {
    let encoded = Zeroizing::new(encoded);
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return Err(PairingCodeError::MalformedCode);
    }
    let (json, v2_envelope): (Zeroizing<String>, bool) =
        if let Some(payload) = trimmed.strip_prefix(REMORA_LINK_V2_ENVELOPE_PREFIX) {
            if payload.len() > MAX_ENCODED_SEGMENT_BYTES {
                return Err(PairingCodeError::CodeTooLarge);
            }
            let mut decoded = URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| PairingCodeError::MalformedCode)?;
            let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(&decoded));
            if canonical.as_str() != payload {
                decoded.zeroize();
                return Err(PairingCodeError::MalformedCode);
            }
            if decoded.len() > MAX_DECODED_JSON_BYTES {
                decoded.zeroize();
                return Err(PairingCodeError::CodeTooLarge);
            }
            let decoded = match String::from_utf8(decoded) {
                Ok(decoded) => decoded,
                Err(error) => {
                    let mut bytes = error.into_bytes();
                    bytes.zeroize();
                    return Err(PairingCodeError::MalformedCode);
                }
            };
            (Zeroizing::new(decoded), true)
        } else if trimmed.starts_with('{') {
            if trimmed.len() > MAX_DECODED_JSON_BYTES {
                return Err(PairingCodeError::CodeTooLarge);
            }
            (Zeroizing::new(trimmed.to_string()), false)
        } else if trimmed.starts_with("remora-link:") {
            // Fail closed for unknown envelope versions. In particular, never
            // reinterpret a broken v2 envelope as a legacy bearer payload.
            return Err(PairingCodeError::IncompatibleProtocol);
        } else {
            // A human-entered short locator is not an authenticator. It stays
            // explicitly unsupported until bilateral confirmation or a PAKE flow
            // is implemented.
            return Err(PairingCodeError::UnsupportedManualCode);
        };

    let version: VersionProbe =
        serde_json::from_str(&json).map_err(|_| PairingCodeError::MalformedCode)?;
    if v2_envelope && version.v != 2 {
        return Err(PairingCodeError::IncompatibleProtocol);
    }
    match version.v {
        LEGACY_V1_PROTOCOL_VERSION => {
            let params = decode_legacy_v1(&json)?;
            Ok(DecodedPairingCode::LegacyV1 { params })
        }
        2 if v2_envelope => {
            decode_v2(&json, now_unix_seconds).map(DecodedPairingCode::DeviceGrantV2)
        }
        2 => Err(PairingCodeError::IncompatibleProtocol),
        _ => Err(PairingCodeError::IncompatibleProtocol),
    }
}

fn decode_v2(json: &str, _now_unix_seconds: u64) -> Result<V2Invite, PairingCodeError> {
    let mut wire: V2InviteWire =
        serde_json::from_str(json).map_err(|_| PairingCodeError::MalformedCode)?;
    if wire.v != 2 {
        return Err(PairingCodeError::IncompatibleProtocol);
    }
    if wire.expires_at < 0
        || u64::try_from(wire.expires_at)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .is_none()
    {
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }
    if wire.node_id.is_empty() {
        return Err(PairingCodeError::MissingHostIdentity);
    }
    if wire.node_id.len() > MAX_ENDPOINT_ID_BYTES
        || wire.node_id.trim() != wire.node_id
        || wire.node_id.chars().any(char::is_control)
    {
        return Err(PairingCodeError::InvalidHostIdentity);
    }
    let node_id = EndpointId::from_str(&wire.node_id)
        .map_err(|_| PairingCodeError::InvalidHostIdentity)?
        .to_string();

    let mut invitation_id = Zeroizing::new(decode_exact_base64url(
        &wire.invitation_id,
        V2_INVITATION_ID_BYTES,
    )?);
    let mut secret = Zeroizing::new(decode_exact_base64url(&wire.secret, V2_SECRET_BYTES)?);
    if validate_policy(
        &wire.max_runtime_ids,
        &wire.max_scopes,
        wire.confirmation_mode,
    )
    .is_err()
    {
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }
    if wire
        .host_name
        .as_deref()
        .is_some_and(|name| !valid_label(name, MAX_HOST_NAME_BYTES))
    {
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }

    let relay = wire.relay.take();
    if let Some(relay) = relay.as_deref() {
        RelayUrl::from_str(relay).map_err(|_| PairingCodeError::InvalidRelayHint)?;
    }

    Ok(V2Invite {
        node_id,
        invitation_id: std::mem::take(&mut *invitation_id),
        secret: std::mem::take(&mut *secret),
        expires_at: wire.expires_at,
        max_runtime_ids: std::mem::take(&mut wire.max_runtime_ids),
        max_scopes: std::mem::take(&mut wire.max_scopes),
        confirmation_mode: wire.confirmation_mode,
        host_name: normalize_host_name(wire.host_name.take()),
        relay,
    })
}

fn decode_exact_base64url(value: &str, expected_bytes: usize) -> Result<Vec<u8>, PairingCodeError> {
    let expected_chars = expected_bytes.saturating_mul(8).div_ceil(6);
    if value.len() != expected_chars || value.contains('=') {
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }
    let mut decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| PairingCodeError::InvalidEnrollmentMaterial)?;
    if decoded.len() != expected_bytes {
        decoded.zeroize();
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }
    let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(&decoded));
    if canonical.as_str() != value {
        decoded.zeroize();
        return Err(PairingCodeError::InvalidEnrollmentMaterial);
    }
    Ok(decoded)
}

fn normalize_host_name(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn sanitize_host_name(value: &str) -> String {
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
        .take(MAX_HOST_NAME_BYTES)
        .collect::<String>()
        .trim()
        .to_string()
}

fn valid_label(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn decode_legacy_v1(json: &str) -> Result<LegacyV1Invitation, PairingCodeError> {
    let wire: LegacyV1InvitationWire =
        serde_json::from_str(json).map_err(|_| PairingCodeError::MalformedCode)?;
    if wire.v != LEGACY_V1_PROTOCOL_VERSION {
        return Err(PairingCodeError::IncompatibleProtocol);
    }
    if wire.node_id.trim().is_empty() {
        return Err(PairingCodeError::MissingHostIdentity);
    }
    EndpointId::from_str(&wire.node_id).map_err(|_| PairingCodeError::InvalidHostIdentity)?;
    if wire.token.trim().is_empty() {
        return Err(PairingCodeError::MissingEnrollmentMaterial);
    }
    if let Some(relay) = wire.relay.as_deref() {
        RelayUrl::from_str(relay).map_err(|_| PairingCodeError::InvalidRelayHint)?;
    }
    Ok(LegacyV1Invitation {
        node_id: wire.node_id.clone(),
        host_name: normalize_host_name(wire.host_name.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_ID: &str = "2c2b9ae25435c9ec1cdcb51d81cdfe30fbc35be9dcb0e8e311725bf233e81d6f";

    fn v2_json(now: u64) -> String {
        serde_json::json!({
            "v": 2,
            "node_id": NODE_ID,
            "invitation_id": URL_SAFE_NO_PAD.encode([7_u8; 16]),
            "secret": URL_SAFE_NO_PAD.encode([9_u8; 32]),
            "expires_at": now + 300,
            "max_runtime_ids": ["codex"],
            "max_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
            "confirmation_mode": "interactive",
            "host_name": "  Studio Mac  ",
            "relay": "https://relay.example"
        })
        .to_string()
    }

    fn v2_envelope(json: impl AsRef<[u8]>) -> String {
        format!(
            "{REMORA_LINK_V2_ENVELOPE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(json.as_ref())
        )
    }

    #[test]
    fn characterizes_existing_v1_wire_and_stable_id() {
        let json = serde_json::json!({
            "v": 1,
            "node_id": NODE_ID,
            "token": "legacy-bearer",
            "host_name": "Old Host",
            "relay": "https://relay.example"
        })
        .to_string();

        let decoded = decode_pairing_code(json, 1_000).expect("legacy code");
        match decoded {
            DecodedPairingCode::LegacyV1 { params } => {
                assert_eq!(params.node_id, NODE_ID);
                assert_eq!(params.host_name.as_deref(), Some("Old Host"));
            }
            DecodedPairingCode::DeviceGrantV2(_) => panic!("expected legacy classification"),
        }
    }

    #[test]
    fn accepts_copy_paste_envelope_and_rejects_raw_v2_json() {
        let now = 5_000;
        let json = v2_json(now);
        let envelope = v2_envelope(&json);
        let copied = decode_pairing_code(envelope, now).expect("copy/paste envelope");

        assert!(matches!(
            decode_pairing_code(json, now),
            Err(PairingCodeError::IncompatibleProtocol)
        ));
        match copied {
            DecodedPairingCode::DeviceGrantV2(invite) => {
                assert_eq!(invite.node_id, NODE_ID);
                assert_eq!(invite.host_name.as_deref(), Some("Studio Mac"));
            }
            DecodedPairingCode::LegacyV1 { .. } => panic!("expected v2 invitation"),
        }
    }

    #[test]
    fn v2_never_falls_back_to_v1() {
        let now = 5_000;
        let mut value: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        value.as_object_mut().unwrap().remove("secret");
        value["token"] = serde_json::Value::String("legacy-bearer".into());

        assert!(matches!(
            decode_pairing_code(v2_envelope(value.to_string()), now),
            Err(PairingCodeError::MalformedCode)
        ));

        let v1 = serde_json::json!({
            "v": 1,
            "node_id": NODE_ID,
            "token": "legacy-bearer"
        })
        .to_string();
        let wrapped_v1 = format!(
            "{REMORA_LINK_V2_ENVELOPE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(v1)
        );
        assert!(matches!(
            decode_pairing_code(wrapped_v1, now),
            Err(PairingCodeError::IncompatibleProtocol)
        ));
    }

    #[test]
    fn rejects_unknown_fields_and_noncanonical_secret_length() {
        let now = 5_000;
        let mut unknown: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        unknown["token"] = serde_json::Value::String("must-not-be-accepted".into());
        assert!(matches!(
            decode_pairing_code(v2_envelope(unknown.to_string()), now),
            Err(PairingCodeError::MalformedCode)
        ));

        let mut short: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        short["secret"] = serde_json::Value::String(URL_SAFE_NO_PAD.encode([1_u8; 31]));
        assert!(matches!(
            decode_pairing_code(v2_envelope(short.to_string()), now),
            Err(PairingCodeError::InvalidEnrollmentMaterial)
        ));
    }

    #[test]
    fn lets_host_decide_expiry_and_rejects_manual_short_codes() {
        let now = 5_000;
        let mut expired: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        expired["expires_at"] = serde_json::Value::from(now);
        assert!(decode_pairing_code(v2_envelope(expired.to_string()), now).is_ok());

        let mut far_future: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        far_future["expires_at"] = serde_json::Value::from(i64::MAX);
        assert!(matches!(
            decode_pairing_code(v2_envelope(far_future.to_string()), now),
            Err(PairingCodeError::InvalidEnrollmentMaterial)
        ));

        assert!(matches!(
            decode_pairing_code("ABCD-EFGH".to_string(), now),
            Err(PairingCodeError::UnsupportedManualCode)
        ));
    }

    #[test]
    fn credential_bearing_debug_output_is_redacted() {
        let decoded = decode_pairing_code(v2_envelope(v2_json(5_000)), 5_000).unwrap();
        let debug = format!("{decoded:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode([9_u8; 32])));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode([7_u8; 16])));
    }
}
