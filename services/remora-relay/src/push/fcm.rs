use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use secrecy::ExposeSecret as _;
use serde::Serialize;

use crate::{PushEnvironment, PushProviderKind};

use super::{BearerTokenFile, ProviderOutcome, PushAttempt, PushProvider, apns::parse_retry_after};

#[derive(Clone, Debug)]
pub struct FcmConfig {
    pub endpoint: Url,
    pub project_id: String,
    pub bearer_token_file: BearerTokenFile,
    pub timeout_ms: u64,
}

pub struct FcmProvider {
    config: FcmConfig,
    client: Client,
}

impl std::fmt::Debug for FcmProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FcmProvider")
            .field("endpoint", &"[fixed FCM endpoint]")
            .field("project_id", &"[redacted]")
            .finish()
    }
}

impl FcmProvider {
    pub fn new(config: FcmConfig) -> std::result::Result<Self, reqwest::Error> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()?;
        Ok(Self { config, client })
    }
}

#[derive(Serialize)]
struct FcmRequest<'a> {
    message: FcmMessage<'a>,
}

#[derive(Serialize)]
struct FcmMessage<'a> {
    // Android registers FIDs; legacy registration tokens require fresh registration.
    fid: &'a str,
    data: FcmData,
    android: FcmAndroid<'a>,
}

#[derive(Serialize)]
struct FcmData {
    schema_version: String,
    installation_id: String,
    event_id: String,
    cursor: String,
    event_class: String,
    expires_at_ms: String,
}

#[derive(Serialize)]
struct FcmAndroid<'a> {
    priority: &'static str,
    ttl: String,
    collapse_key: &'a str,
}

pub(crate) fn payload_json(attempt: &PushAttempt, now_ms: i64) -> serde_json::Result<Vec<u8>> {
    let ttl_seconds = attempt
        .hint
        .expires_at_ms
        .saturating_sub(now_ms)
        .max(1_000)
        .div_euclid(1_000);
    serde_json::to_vec(&FcmRequest {
        message: FcmMessage {
            fid: attempt.token.expose_secret(),
            data: FcmData {
                schema_version: attempt.hint.schema_version.to_string(),
                installation_id: attempt.hint.installation_id.as_str().to_owned(),
                event_id: attempt.hint.event_id.as_str().to_owned(),
                cursor: attempt.hint.cursor.to_string(),
                event_class: attempt.hint.event_class.as_str().to_owned(),
                expires_at_ms: attempt.hint.expires_at_ms.to_string(),
            },
            android: FcmAndroid {
                priority: "NORMAL",
                ttl: format!("{ttl_seconds}s"),
                collapse_key: attempt.hint.event_class.as_str(),
            },
        },
    })
}

#[async_trait]
impl PushProvider for FcmProvider {
    fn kind(&self) -> PushProviderKind {
        PushProviderKind::Fcm
    }

    fn environment(&self) -> PushEnvironment {
        PushEnvironment::Production
    }

    async fn send(&self, attempt: &PushAttempt, now_ms: i64) -> ProviderOutcome {
        let bearer = match self.config.bearer_token_file.load().await {
            Ok(bearer) => bearer,
            Err(_) => {
                return ProviderOutcome::Retry {
                    retry_after_ms: None,
                };
            }
        };
        let body = match payload_json(attempt, now_ms) {
            Ok(body) => body,
            Err(_) => return ProviderOutcome::PermanentFailure,
        };
        let mut url = self.config.endpoint.clone();
        url.set_path(&format!(
            "/v1/projects/{}/messages:send",
            self.config.project_id
        ));
        let response = self
            .client
            .post(url)
            .bearer_auth(bearer.expose_secret())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await;
        let Ok(mut response) = response else {
            return ProviderOutcome::Retry {
                retry_after_ms: None,
            };
        };
        match response.status() {
            StatusCode::OK => ProviderOutcome::Accepted,
            status if is_retryable_credential_status(status) => ProviderOutcome::Retry {
                retry_after_ms: None,
            },
            StatusCode::TOO_MANY_REQUESTS => ProviderOutcome::Retry {
                retry_after_ms: parse_retry_after(response.headers()),
            },
            status if status.is_server_error() => ProviderOutcome::Retry {
                retry_after_ms: parse_retry_after(response.headers()),
            },
            StatusCode::NOT_FOUND => {
                let mut body = Vec::new();
                while body.len() <= 16 * 1_024 {
                    match response.chunk().await {
                        Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                        Ok(None) => break,
                        Err(_) => return ProviderOutcome::PermanentFailure,
                    }
                }
                if body.len() <= 16 * 1_024 && contains_unregistered_code(&body) {
                    ProviderOutcome::InvalidToken
                } else {
                    ProviderOutcome::PermanentFailure
                }
            }
            _ => ProviderOutcome::PermanentFailure,
        }
    }
}

fn is_retryable_credential_status(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

fn contains_unregistered_code(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value
        .get("error")
        .and_then(|error| error.get("details"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                detail.get("@type").and_then(serde_json::Value::as_str)
                    == Some("type.googleapis.com/google.firebase.fcm.v1.FcmError")
                    && detail.get("errorCode").and_then(serde_json::Value::as_str)
                        == Some("UNREGISTERED")
            })
        })
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::{EventClass, OpaqueId, OpaqueWakeHint, SCHEMA_VERSION};

    #[test]
    fn fcm_data_has_only_opaque_contract_fields() {
        let attempt = PushAttempt {
            token: SecretString::from("c0123456789abcdefghijk".to_owned()),
            hint: OpaqueWakeHint {
                schema_version: SCHEMA_VERSION,
                installation_id: OpaqueId::parse("inst_0123456789abcdef").unwrap(),
                event_id: OpaqueId::parse("evt_0123456789abcdef0").unwrap(),
                cursor: 44,
                event_class: EventClass::SecurityChanged,
                expires_at_ms: 123_456,
            },
        };
        let value: serde_json::Value =
            serde_json::from_slice(&payload_json(&attempt, 100_000).unwrap()).unwrap();
        let data = value["message"]["data"].as_object().unwrap();
        let message_keys: std::collections::BTreeSet<_> = value["message"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            message_keys,
            ["android", "data", "fid"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(value["message"]["fid"], "c0123456789abcdefghijk");
        assert!(value["message"].get("token").is_none());
        assert_ne!(
            value["message"]["fid"],
            value["message"]["data"]["installation_id"]
        );
        let keys: std::collections::BTreeSet<_> = data.keys().cloned().collect();
        assert_eq!(
            keys,
            [
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
        assert!(!serialized.contains("prompt"));
        assert!(!serialized.contains("transcript"));
        assert!(!serialized.contains("host_id"));
        assert!(!serialized.contains("notification"));
        assert!(!serialized.contains("click_action"));
    }

    #[test]
    fn only_explicit_unregistered_code_tombstones_token() {
        assert!(contains_unregistered_code(
            br#"{"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#
        ));
        for body in [
            r#"{"error":{"status":"NOT_FOUND"}}"#,
            r#"{"error":{"message":"UNREGISTERED"}}"#,
            r#"{"error":{"details":[{"errorCode":"UNREGISTERED"}]}}"#,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.BadRequest","errorCode":"UNREGISTERED"}]}}"#,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","nested":{"errorCode":"UNREGISTERED"}}]}}"#,
            r#"{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}"#,
        ] {
            assert!(!contains_unregistered_code(body.as_bytes()), "{body}");
        }
    }

    #[tokio::test]
    async fn missing_refreshable_credential_is_retryable_without_network() {
        let directory = tempfile::tempdir().unwrap();
        let provider = FcmProvider::new(FcmConfig {
            endpoint: Url::parse("https://fcm.googleapis.com").unwrap(),
            project_id: "remora-project".into(),
            bearer_token_file: BearerTokenFile::new(directory.path().join("missing.oauth")),
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
}
