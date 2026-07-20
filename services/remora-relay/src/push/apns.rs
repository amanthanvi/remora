use std::time::Duration;

use async_trait::async_trait;
use reqwest::{
    Client, StatusCode, Url,
    header::{HeaderMap, HeaderValue, RETRY_AFTER},
    redirect::Policy,
};
use secrecy::ExposeSecret as _;
use serde::Serialize;

use crate::{EventClass, OpaqueId, PushEnvironment, PushProviderKind};

use super::{BearerTokenFile, ProviderOutcome, PushAttempt, PushProvider};

#[derive(Clone, Debug)]
pub struct ApnsConfig {
    pub endpoint: Url,
    pub topic: String,
    pub bearer_token_file: BearerTokenFile,
    pub environment: PushEnvironment,
    pub timeout_ms: u64,
}

pub struct ApnsProvider {
    config: ApnsConfig,
    client: Client,
}

impl std::fmt::Debug for ApnsProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApnsProvider")
            .field("endpoint", &"[fixed APNs endpoint]")
            .field("topic", &"[redacted]")
            .finish()
    }
}

impl ApnsProvider {
    pub fn new(config: ApnsConfig) -> std::result::Result<Self, reqwest::Error> {
        let client = Client::builder()
            .http2_adaptive_window(true)
            .redirect(Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()?;
        Ok(Self { config, client })
    }
}

#[derive(Serialize)]
struct Aps {
    #[serde(rename = "content-available")]
    content_available: u8,
}

#[derive(Serialize)]
struct ApnsPayload<'a> {
    aps: Aps,
    schema_version: u16,
    installation_id: &'a OpaqueId,
    event_id: &'a OpaqueId,
    cursor: u64,
    event_class: EventClass,
    expires_at_ms: i64,
}

pub(crate) fn payload_json(attempt: &PushAttempt) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&ApnsPayload {
        aps: Aps {
            content_available: 1,
        },
        schema_version: attempt.hint.schema_version,
        installation_id: &attempt.hint.installation_id,
        event_id: &attempt.hint.event_id,
        cursor: attempt.hint.cursor,
        event_class: attempt.hint.event_class,
        expires_at_ms: attempt.hint.expires_at_ms,
    })
}

#[async_trait]
impl PushProvider for ApnsProvider {
    fn kind(&self) -> PushProviderKind {
        PushProviderKind::Apns
    }

    fn environment(&self) -> PushEnvironment {
        self.config.environment
    }

    async fn send(&self, attempt: &PushAttempt, _now_ms: i64) -> ProviderOutcome {
        let bearer = match self.config.bearer_token_file.load().await {
            Ok(bearer) => bearer,
            Err(_) => {
                return ProviderOutcome::Retry {
                    retry_after_ms: None,
                };
            }
        };
        let body = match payload_json(attempt) {
            Ok(body) => body,
            Err(_) => return ProviderOutcome::PermanentFailure,
        };
        let mut url = self.config.endpoint.clone();
        let token = attempt.token.expose_secret();
        let path = format!("{}/3/device/{token}", url.path().trim_end_matches('/'));
        url.set_path(&path);
        let expiry_seconds = attempt.hint.expires_at_ms.div_euclid(1_000);
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("apns-topic", self.config.topic.as_str()),
            ("apns-push-type", "background"),
            ("apns-priority", "5"),
            ("apns-collapse-id", attempt.hint.event_class.as_str()),
        ] {
            let Ok(value) = HeaderValue::from_str(value) else {
                return ProviderOutcome::PermanentFailure;
            };
            headers.insert(name, value);
        }
        let Ok(expiration) = HeaderValue::from_str(&expiry_seconds.to_string()) else {
            return ProviderOutcome::PermanentFailure;
        };
        headers.insert("apns-expiration", expiration);

        let response = self
            .client
            .post(url)
            .bearer_auth(bearer.expose_secret())
            .headers(headers)
            .body(body)
            .send()
            .await;
        classify_response(response).await
    }
}

async fn classify_response(
    response: std::result::Result<reqwest::Response, reqwest::Error>,
) -> ProviderOutcome {
    let Ok(mut response) = response else {
        return ProviderOutcome::Retry {
            retry_after_ms: None,
        };
    };
    match response.status() {
        StatusCode::OK => ProviderOutcome::Accepted,
        StatusCode::GONE => ProviderOutcome::InvalidToken,
        StatusCode::BAD_REQUEST => {
            let mut body = Vec::new();
            while body.len() <= 16 * 1_024 {
                match response.chunk().await {
                    Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(_) => return ProviderOutcome::PermanentFailure,
                }
            }
            if body.len() <= 16 * 1_024 && contains_invalid_destination_reason(&body) {
                ProviderOutcome::InvalidToken
            } else {
                ProviderOutcome::PermanentFailure
            }
        }
        status if is_retryable_credential_status(status) => ProviderOutcome::Retry {
            retry_after_ms: None,
        },
        StatusCode::TOO_MANY_REQUESTS => ProviderOutcome::Retry {
            retry_after_ms: parse_retry_after(response.headers()),
        },
        status if status.is_server_error() => ProviderOutcome::Retry {
            retry_after_ms: parse_retry_after(response.headers()),
        },
        _ => ProviderOutcome::PermanentFailure,
    }
}

fn contains_invalid_destination_reason(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    matches!(
        value.get("reason").and_then(serde_json::Value::as_str),
        Some("BadDeviceToken" | "DeviceTokenNotForTopic")
    )
}

fn is_retryable_credential_status(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000))
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::{OpaqueWakeHint, SCHEMA_VERSION};

    #[test]
    fn apns_payload_has_only_aps_and_opaque_contract_fields() {
        let attempt = PushAttempt {
            token: SecretString::from("provider-token-canary".to_owned()),
            hint: OpaqueWakeHint {
                schema_version: SCHEMA_VERSION,
                installation_id: OpaqueId::parse("inst_0123456789abcdef").unwrap(),
                event_id: OpaqueId::parse("evt_0123456789abcdef0").unwrap(),
                cursor: 44,
                event_class: EventClass::ActivityChanged,
                expires_at_ms: 123_456,
            },
        };
        let value: serde_json::Value =
            serde_json::from_slice(&payload_json(&attempt).unwrap()).unwrap();
        let keys: std::collections::BTreeSet<_> =
            value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "aps",
                "cursor",
                "event_class",
                "event_id",
                "expires_at_ms",
                "installation_id",
                "schema_version",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        let serialized = value.to_string();
        assert!(!serialized.contains("provider-token-canary"));
        assert!(!serialized.contains("prompt"));
        assert!(!serialized.contains("transcript"));
    }

    #[tokio::test]
    async fn missing_refreshable_credential_is_retryable_without_network() {
        let directory = tempfile::tempdir().unwrap();
        let provider = ApnsProvider::new(ApnsConfig {
            endpoint: Url::parse("https://api.push.apple.com").unwrap(),
            topic: "com.remora.app".into(),
            bearer_token_file: BearerTokenFile::new(directory.path().join("missing.jwt")),
            environment: PushEnvironment::Production,
            timeout_ms: 1_000,
        })
        .unwrap();
        let attempt = PushAttempt {
            token: SecretString::from("provider-token-canary".to_owned()),
            hint: OpaqueWakeHint {
                schema_version: SCHEMA_VERSION,
                installation_id: OpaqueId::parse("inst_0123456789abcdef").unwrap(),
                event_id: OpaqueId::parse("evt_0123456789abcdef0").unwrap(),
                cursor: 1,
                event_class: EventClass::StateChanged,
                expires_at_ms: 123_456,
            },
        };
        assert!(matches!(
            provider.send(&attempt, 100_000).await,
            ProviderOutcome::Retry { .. }
        ));
    }

    #[test]
    fn provider_credential_statuses_are_retryable() {
        assert!(is_retryable_credential_status(StatusCode::UNAUTHORIZED));
        assert!(is_retryable_credential_status(StatusCode::FORBIDDEN));
        assert!(!is_retryable_credential_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn only_explicit_apns_destination_reasons_tombstone_tokens() {
        assert!(contains_invalid_destination_reason(
            br#"{"reason":"BadDeviceToken"}"#
        ));
        assert!(contains_invalid_destination_reason(
            br#"{"reason":"DeviceTokenNotForTopic"}"#
        ));
        assert!(!contains_invalid_destination_reason(
            br#"{"reason":"BadCollapseId"}"#
        ));
        assert!(!contains_invalid_destination_reason(
            br#"{"nested":{"reason":"BadDeviceToken"}}"#
        ));
    }
}
