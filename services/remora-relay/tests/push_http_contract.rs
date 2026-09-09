use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    routing::any,
};
use remora_relay::{
    EventClass, OpaqueId, OpaqueWakeHint, ProviderOutcome, PushAttempt, PushEnvironment,
    SCHEMA_VERSION,
    push::{ApnsConfig, ApnsProvider, BearerTokenFile, FcmConfig, FcmProvider, PushProvider},
};
use secrecy::SecretString;
use tokio::{net::TcpListener, sync::mpsc, task::JoinSet};

#[tokio::test]
async fn provider_http_contracts_reload_credentials_and_classify_responses() {
    for apns in [false, true] {
        let response = Arc::new(Mutex::new((StatusCode::OK, String::new())));
        let (sent, mut received) = mpsc::unbounded_channel();
        let app = Router::new()
            .fallback(any(
                move |State(response): State<Arc<Mutex<(StatusCode, String)>>>,
                      method: Method,
                      uri: Uri,
                      headers: HeaderMap,
                      body: Bytes| {
                    let sent = sent.clone();
                    async move {
                        sent.send((method, uri, headers, body)).unwrap();
                        let (status, body) = response.lock().unwrap().clone();
                        (
                            status,
                            [("retry-after", "2"), ("location", "/redirected")],
                            body,
                        )
                    }
                },
            ))
            .with_state(response.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let mut server = JoinSet::new();
        server.spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = tempfile::tempdir().unwrap();
        let token_path = directory.path().join("provider-token");
        let token_file = BearerTokenFile::new(&token_path);
        // Only these directly constructed test adapters use loopback. Production
        // configuration keeps provider endpoints fixed and credentials external.
        let provider: Box<dyn PushProvider> = if apns {
            Box::new(
                ApnsProvider::new(ApnsConfig {
                    endpoint,
                    topic: "com.remora.app".into(),
                    bearer_token_file: token_file,
                    environment: PushEnvironment::Sandbox,
                    timeout_ms: 1_000,
                })
                .unwrap(),
            )
        } else {
            Box::new(
                FcmProvider::new(FcmConfig {
                    endpoint,
                    project_id: "remora-test-project".into(),
                    bearer_token_file: token_file,
                    timeout_ms: 1_000,
                })
                .unwrap(),
            )
        };
        let destination = if apns {
            "synthetic-device-token-00000000000"
        } else {
            "c0123456789abcdefghijk"
        };
        let attempt = PushAttempt {
            token: SecretString::from(destination.to_owned()),
            hint: OpaqueWakeHint {
                schema_version: SCHEMA_VERSION,
                installation_id: OpaqueId::parse("inst_0123456789abcdef").unwrap(),
                event_id: OpaqueId::parse("evt_0123456789abcdef0").unwrap(),
                cursor: 44,
                event_class: EventClass::StateChanged,
                expires_at_ms: 123_456,
            },
        };
        let invalid_status = if apns {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::NOT_FOUND
        };
        let invalid_body = if apns {
            r#"{"reason":"BadDeviceToken"}"#
        } else {
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#
        };
        let cases = [
            (StatusCode::OK, "{}", ProviderOutcome::Accepted),
            (
                StatusCode::UNAUTHORIZED,
                "{}",
                ProviderOutcome::Retry {
                    retry_after_ms: None,
                },
            ),
            (
                StatusCode::FORBIDDEN,
                "{}",
                ProviderOutcome::Retry {
                    retry_after_ms: None,
                },
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "{}",
                ProviderOutcome::Retry {
                    retry_after_ms: Some(2_000),
                },
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "{}",
                ProviderOutcome::Retry {
                    retry_after_ms: Some(2_000),
                },
            ),
            (
                invalid_status,
                r#"{"error":{"message":"UNREGISTERED"},"nested":{"reason":"BadDeviceToken"}}"#,
                ProviderOutcome::PermanentFailure,
            ),
            (
                invalid_status,
                "not-json",
                ProviderOutcome::PermanentFailure,
            ),
            (invalid_status, invalid_body, ProviderOutcome::InvalidToken),
            (
                StatusCode::TEMPORARY_REDIRECT,
                "{}",
                ProviderOutcome::PermanentFailure,
            ),
        ];
        for (index, (status, body, expected)) in cases.into_iter().enumerate() {
            let bearer = format!("synthetic-refreshable-bearer-token-{index:04}");
            tokio::fs::write(&token_path, format!("{bearer}\n"))
                .await
                .unwrap();
            *response.lock().unwrap() = (status, body.to_owned());
            assert_eq!(provider.send(&attempt, 100_000).await, expected);
            let (method, uri, headers, body) = received.try_recv().unwrap();
            assert_eq!(method, Method::POST);
            assert_eq!(headers["authorization"], format!("Bearer {bearer}"));
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if apns {
                assert_eq!(uri.path(), "/3/device/synthetic-device-token-00000000000");
                assert_eq!(headers["apns-topic"], "com.remora.app");
                assert_eq!(headers["apns-push-type"], "background");
                assert_eq!(headers["apns-priority"], "5");
                assert_eq!(headers["apns-expiration"], "123");
                assert_eq!(headers["apns-collapse-id"], "state_changed");
                assert_eq!(body["aps"]["content-available"], 1);
                assert_eq!(body["cursor"], 44);
            } else {
                assert_eq!(uri.path(), "/v1/projects/remora-test-project/messages:send");
                assert_eq!(headers["content-type"], "application/json");
                assert_eq!(body["message"]["fid"], "c0123456789abcdefghijk");
                assert!(body["message"].get("token").is_none());
                assert_eq!(body["message"]["data"]["cursor"], "44");
                assert_eq!(body["message"]["android"]["ttl"], "23s");
                assert_eq!(body["message"]["android"]["priority"], "NORMAL");
            }
            assert!(
                received.try_recv().is_err(),
                "redirect must not be followed"
            );
        }
        tokio::fs::remove_file(&token_path).await.unwrap();
        assert_eq!(
            provider.send(&attempt, 100_000).await,
            ProviderOutcome::Retry {
                retry_after_ms: None
            }
        );
        assert!(received.try_recv().is_err());
        server.shutdown().await;
    }
}
