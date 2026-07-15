use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::{EndpointId, RelayUrl};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use super::types::{
    RemoteHostId, RemoteHostPairingError, RemotePairingCodeInspection,
    RemotePairingOfferDisposition, RemotePairingProtocol,
};

pub(crate) const REMORA_LINK_V2_ALPN: &[u8] = b"remora-link/2";
const REMORA_LINK_V2_ENVELOPE_PREFIX: &str = "remora-link:v2:";
const MAX_ENCODED_CODE_BYTES: usize = 16 * 1024;
const MAX_DECODED_JSON_BYTES: usize = 8 * 1024;
const V2_INVITATION_ID_BYTES: usize = 16;
const V2_SECRET_BYTES: usize = 32;
const MAX_V2_OFFER_LIFETIME_SECONDS: u64 = 15 * 60;
const MAX_HOST_NAME_CHARS: usize = 96;

/// Private, credential-bearing result of decoding a code. Its custom Debug
/// implementation is intentionally redacted.
#[derive(Clone)]
pub(crate) enum DecodedPairingCode {
    LegacyV1 {
        params: crate::alleycat::ParsedPairPayload,
    },
    DeviceGrantV2(V2Invite),
}

impl DecodedPairingCode {
    pub(crate) fn host_id(&self) -> RemoteHostId {
        match self {
            Self::LegacyV1 { params } => RemoteHostId {
                // Existing profiles and ThreadKey values depend on this exact
                // wire-compatibility ID. Do not rename it during the v2 cutover.
                value: format!("alleycat:{}", params.node_id),
            },
            Self::DeviceGrantV2(invite) => RemoteHostId {
                value: format!("remora-link:{}", invite.node_id),
            },
        }
    }

    pub(crate) fn protocol(&self) -> RemotePairingProtocol {
        match self {
            Self::LegacyV1 { .. } => RemotePairingProtocol::LegacyV1,
            Self::DeviceGrantV2(_) => RemotePairingProtocol::DeviceGrantV2,
        }
    }

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

    pub(crate) fn expires_at_unix_ms(&self) -> Option<u64> {
        match self {
            Self::LegacyV1 { .. } => None,
            Self::DeviceGrantV2(invite) => invite.expires_at.checked_mul(1_000),
        }
    }

    pub(crate) fn inspection(&self) -> RemotePairingCodeInspection {
        let protocol = self.protocol();
        RemotePairingCodeInspection {
            host_id: self.host_id(),
            suggested_display_name: self.suggested_display_name(),
            protocol,
            disposition: match protocol {
                RemotePairingProtocol::LegacyV1 => RemotePairingOfferDisposition::RePairRequired,
                RemotePairingProtocol::DeviceGrantV2 => RemotePairingOfferDisposition::Ready,
            },
            expires_at_unix_ms: self.expires_at_unix_ms(),
        }
    }
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
    pub(crate) expires_at: u64,
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
#[serde(deny_unknown_fields)]
struct V2InviteWire {
    v: u32,
    node_id: String,
    invitation_id: String,
    secret: String,
    expires_at: u64,
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
) -> Result<DecodedPairingCode, RemoteHostPairingError> {
    let encoded = Zeroizing::new(encoded);
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return Err(RemoteHostPairingError::MalformedCode);
    }
    if trimmed.len() > MAX_ENCODED_CODE_BYTES {
        return Err(RemoteHostPairingError::CodeTooLarge);
    }

    let (json, v2_envelope): (Zeroizing<String>, bool) =
        if let Some(payload) = trimmed.strip_prefix(REMORA_LINK_V2_ENVELOPE_PREFIX) {
            let mut decoded = URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| RemoteHostPairingError::MalformedCode)?;
            if decoded.len() > MAX_DECODED_JSON_BYTES {
                decoded.zeroize();
                return Err(RemoteHostPairingError::CodeTooLarge);
            }
            let decoded = match String::from_utf8(decoded) {
                Ok(decoded) => decoded,
                Err(error) => {
                    let mut bytes = error.into_bytes();
                    bytes.zeroize();
                    return Err(RemoteHostPairingError::MalformedCode);
                }
            };
            (Zeroizing::new(decoded), true)
        } else if trimmed.starts_with('{') {
            if trimmed.len() > MAX_DECODED_JSON_BYTES {
                return Err(RemoteHostPairingError::CodeTooLarge);
            }
            (Zeroizing::new(trimmed.to_string()), false)
        } else if trimmed.starts_with("remora-link:") {
            // Fail closed for unknown envelope versions. In particular, never
            // reinterpret a broken v2 envelope as a legacy bearer payload.
            return Err(RemoteHostPairingError::IncompatibleProtocol);
        } else {
            // A human-entered short locator is not an authenticator. It stays
            // explicitly unsupported until bilateral confirmation or a PAKE flow
            // is implemented.
            return Err(RemoteHostPairingError::UnsupportedManualCode);
        };

    let version: VersionProbe =
        serde_json::from_str(&json).map_err(|_| RemoteHostPairingError::MalformedCode)?;
    if v2_envelope && version.v != 2 {
        return Err(RemoteHostPairingError::IncompatibleProtocol);
    }
    match version.v {
        crate::alleycat::ALLEYCAT_PROTOCOL_VERSION => {
            let mut params = crate::alleycat::parse_pair_payload(&json).map_err(map_v1_error)?;
            // The v2 workflow retains only the classification fields. Existing
            // v1 sessions continue through their separate compatibility path.
            params.token.zeroize();
            Ok(DecodedPairingCode::LegacyV1 { params })
        }
        2 => decode_v2(&json, now_unix_seconds).map(DecodedPairingCode::DeviceGrantV2),
        _ => Err(RemoteHostPairingError::IncompatibleProtocol),
    }
}

fn decode_v2(json: &str, now_unix_seconds: u64) -> Result<V2Invite, RemoteHostPairingError> {
    let mut wire: V2InviteWire =
        serde_json::from_str(json).map_err(|_| RemoteHostPairingError::MalformedCode)?;
    if wire.v != 2 {
        return Err(RemoteHostPairingError::IncompatibleProtocol);
    }
    let node_id = wire.node_id.trim();
    if node_id.is_empty() {
        return Err(RemoteHostPairingError::MissingHostIdentity);
    }
    let node_id = EndpointId::from_str(node_id)
        .map_err(|_| RemoteHostPairingError::InvalidHostIdentity)?
        .to_string();

    let mut invitation_id = Zeroizing::new(decode_exact_base64url(
        &wire.invitation_id,
        V2_INVITATION_ID_BYTES,
    )?);
    let mut secret = Zeroizing::new(decode_exact_base64url(&wire.secret, V2_SECRET_BYTES)?);
    if wire.expires_at <= now_unix_seconds {
        return Err(RemoteHostPairingError::OfferExpired);
    }
    if wire.expires_at.saturating_sub(now_unix_seconds) > MAX_V2_OFFER_LIFETIME_SECONDS {
        return Err(RemoteHostPairingError::InvalidEnrollmentMaterial);
    }

    let relay = wire
        .relay
        .take()
        .map(|relay| relay.trim().to_string())
        .filter(|relay| !relay.is_empty());
    if let Some(relay) = relay.as_deref() {
        RelayUrl::from_str(relay).map_err(|_| RemoteHostPairingError::InvalidRelayHint)?;
    }

    Ok(V2Invite {
        node_id,
        invitation_id: std::mem::take(&mut *invitation_id),
        secret: std::mem::take(&mut *secret),
        expires_at: wire.expires_at,
        host_name: normalize_host_name(wire.host_name.take()),
        relay,
    })
}

fn decode_exact_base64url(
    value: &str,
    expected_bytes: usize,
) -> Result<Vec<u8>, RemoteHostPairingError> {
    let expected_chars = expected_bytes.saturating_mul(8).div_ceil(6);
    if value.len() != expected_chars || value.contains('=') {
        return Err(RemoteHostPairingError::InvalidEnrollmentMaterial);
    }
    let mut decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| RemoteHostPairingError::InvalidEnrollmentMaterial)?;
    if decoded.len() != expected_bytes {
        decoded.zeroize();
        return Err(RemoteHostPairingError::InvalidEnrollmentMaterial);
    }
    Ok(decoded)
}

fn normalize_host_name(value: Option<String>) -> Option<String> {
    value
        .map(|value| sanitize_host_name(&value))
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
        .take(MAX_HOST_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn map_v1_error(error: crate::alleycat::AlleycatError) -> RemoteHostPairingError {
    match error {
        crate::alleycat::AlleycatError::ProtocolMismatch { .. } => {
            RemoteHostPairingError::IncompatibleProtocol
        }
        crate::alleycat::AlleycatError::Transport(_) => RemoteHostPairingError::HostUnavailable,
        crate::alleycat::AlleycatError::InvalidPayload(message) => {
            if message.contains("empty node_id") {
                RemoteHostPairingError::MissingHostIdentity
            } else if message.contains("invalid node_id") {
                RemoteHostPairingError::InvalidHostIdentity
            } else if message.contains("empty token") {
                RemoteHostPairingError::MissingEnrollmentMaterial
            } else if message.contains("relay URL") {
                RemoteHostPairingError::InvalidRelayHint
            } else {
                RemoteHostPairingError::MalformedCode
            }
        }
    }
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
            "host_name": "  Studio Mac  ",
            "relay": "https://relay.example"
        })
        .to_string()
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
        let inspection = decoded.inspection();
        assert_eq!(inspection.protocol, RemotePairingProtocol::LegacyV1);
        assert_eq!(
            inspection.disposition,
            RemotePairingOfferDisposition::RePairRequired
        );
        assert_eq!(inspection.host_id.value, format!("alleycat:{NODE_ID}"));
        assert_eq!(inspection.suggested_display_name, "Old Host");
        assert_eq!(inspection.expires_at_unix_ms, None);
    }

    #[test]
    fn accepts_same_v2_offer_as_raw_json_and_copy_paste_envelope() {
        let now = 5_000;
        let json = v2_json(now);
        let envelope = format!(
            "{REMORA_LINK_V2_ENVELOPE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(json.as_bytes())
        );
        let raw = decode_pairing_code(json, now).expect("raw JSON");
        let copied = decode_pairing_code(envelope, now).expect("copy/paste envelope");

        assert_eq!(raw.inspection(), copied.inspection());
        assert_eq!(
            raw.inspection().host_id.value,
            format!("remora-link:{NODE_ID}")
        );
        assert_eq!(raw.inspection().suggested_display_name, "Studio Mac");
    }

    #[test]
    fn v2_never_falls_back_to_v1() {
        let now = 5_000;
        let mut value: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        value.as_object_mut().unwrap().remove("secret");
        value["token"] = serde_json::Value::String("legacy-bearer".into());

        assert!(matches!(
            decode_pairing_code(value.to_string(), now),
            Err(RemoteHostPairingError::MalformedCode)
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
            Err(RemoteHostPairingError::IncompatibleProtocol)
        ));
    }

    #[test]
    fn rejects_unknown_fields_and_noncanonical_secret_length() {
        let now = 5_000;
        let mut unknown: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        unknown["token"] = serde_json::Value::String("must-not-be-accepted".into());
        assert!(matches!(
            decode_pairing_code(unknown.to_string(), now),
            Err(RemoteHostPairingError::MalformedCode)
        ));

        let mut short: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        short["secret"] = serde_json::Value::String(URL_SAFE_NO_PAD.encode([1_u8; 31]));
        assert!(matches!(
            decode_pairing_code(short.to_string(), now),
            Err(RemoteHostPairingError::InvalidEnrollmentMaterial)
        ));
    }

    #[test]
    fn rejects_expired_far_future_and_manual_short_codes() {
        let now = 5_000;
        let mut expired: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        expired["expires_at"] = serde_json::Value::from(now);
        assert!(matches!(
            decode_pairing_code(expired.to_string(), now),
            Err(RemoteHostPairingError::OfferExpired)
        ));

        let mut far_future: serde_json::Value = serde_json::from_str(&v2_json(now)).unwrap();
        far_future["expires_at"] = serde_json::Value::from(now + MAX_V2_OFFER_LIFETIME_SECONDS + 1);
        assert!(matches!(
            decode_pairing_code(far_future.to_string(), now),
            Err(RemoteHostPairingError::InvalidEnrollmentMaterial)
        ));

        assert!(matches!(
            decode_pairing_code("ABCD-EFGH".to_string(), now),
            Err(RemoteHostPairingError::UnsupportedManualCode)
        ));
    }

    #[test]
    fn credential_bearing_debug_output_is_redacted() {
        let decoded = decode_pairing_code(v2_json(5_000), 5_000).unwrap();
        let debug = format!("{decoded:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode([9_u8; 32])));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode([7_u8; 16])));
    }
}
