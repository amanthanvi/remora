use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    BootstrapAuth, IngestEventRequest, OpaqueId, PresentedCapability, RegisterDeviceRequest,
    RelayBackend, RelayError, RelayMetrics, Result, TombstoneRegistrationRequest,
    config::bootstrap_hash, worker::unix_time_ms,
};

#[derive(Clone, Debug)]
pub struct ApiState {
    pub backend: RelayBackend,
    pub metrics: Arc<RelayMetrics>,
    pub bootstrap_auth: BootstrapAuth,
    pub max_body_bytes: usize,
}

pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/v1/installations", post(create_installation))
        .route(
            "/v1/installations/{installation_id}",
            delete(tombstone_installation),
        )
        .route(
            "/v1/installations/{installation_id}/events",
            post(ingest_event).get(fetch_events),
        )
        .route(
            "/v1/installations/{installation_id}/snapshot",
            get(fetch_snapshot),
        )
        .route(
            "/v1/installations/{installation_id}/devices",
            post(register_device),
        )
        .route(
            "/v1/installations/{installation_id}/devices/{registration_id}",
            delete(tombstone_registration),
        )
        .layer(DefaultBodyLimit::max(state.max_body_bytes))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<ApiState>) -> StatusCode {
    if state.backend.ready().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn metrics(State(state): State<ApiState>) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        state.metrics.render_prometheus(),
    )
        .into_response()
}

async fn create_installation(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Response> {
    authorize_bootstrap(&state.bootstrap_auth, &headers)?;
    let installation = state.backend.create_installation(unix_time_ms()).await?;
    Ok((StatusCode::CREATED, Json(installation)).into_response())
}

async fn ingest_event(
    State(state): State<ApiState>,
    Path(installation_id): Path<String>,
    headers: HeaderMap,
    payload: std::result::Result<Json<IngestEventRequest>, JsonRejection>,
) -> Result<Response> {
    let Json(request) = payload.map_err(|_| RelayError::Invalid("JSON body"))?;
    let installation_id = OpaqueId::parse(installation_id)?;
    let capability = bearer_capability(&headers)?;
    let response = state
        .backend
        .ingest_event(installation_id, capability, request, unix_time_ms())
        .await?;
    let status = if response.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(response)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_page_limit")]
    limit: u32,
}

async fn fetch_events(
    State(state): State<ApiState>,
    Path(installation_id): Path<String>,
    query: std::result::Result<Query<FetchQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Json<crate::EventPage>> {
    let Query(query) = query.map_err(|_| RelayError::Invalid("query"))?;
    let page = state
        .backend
        .fetch_events(
            OpaqueId::parse(installation_id)?,
            bearer_capability(&headers)?,
            query.after,
            query.limit,
            unix_time_ms(),
        )
        .await?;
    Ok(Json(page))
}

async fn fetch_snapshot(
    State(state): State<ApiState>,
    Path(installation_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<crate::SnapshotEnvelope>> {
    let snapshot = state
        .backend
        .fetch_snapshot(
            OpaqueId::parse(installation_id)?,
            bearer_capability(&headers)?,
            unix_time_ms(),
        )
        .await?;
    Ok(Json(snapshot))
}

async fn register_device(
    State(state): State<ApiState>,
    Path(installation_id): Path<String>,
    headers: HeaderMap,
    payload: std::result::Result<Json<RegisterDeviceRequest>, JsonRejection>,
) -> Result<Response> {
    let Json(mut request) = payload.map_err(|_| RelayError::Invalid("JSON body"))?;
    let token = Zeroizing::new(std::mem::take(&mut request.token));
    let result = state
        .backend
        .register_device(
            OpaqueId::parse(installation_id)?,
            bearer_capability(&headers)?,
            request.provider,
            request.environment,
            token,
            unix_time_ms(),
        )
        .await;
    let response = result?;
    let status = if response.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TombstoneQuery {
    through_generation: u64,
}

async fn tombstone_registration(
    State(state): State<ApiState>,
    Path((installation_id, registration_id)): Path<(String, String)>,
    query: std::result::Result<Query<TombstoneQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let Query(query) = query.map_err(|_| RelayError::Invalid("query"))?;
    let request = TombstoneRegistrationRequest {
        through_generation: query.through_generation,
    };
    if request.through_generation == 0 {
        return Err(RelayError::Invalid("registration generation"));
    }
    state
        .backend
        .tombstone_registration(
            OpaqueId::parse(installation_id)?,
            bearer_capability(&headers)?,
            OpaqueId::parse(registration_id)?,
            request.through_generation,
            unix_time_ms(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn tombstone_installation(
    State(state): State<ApiState>,
    Path(installation_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    state
        .backend
        .tombstone_installation(
            OpaqueId::parse(installation_id)?,
            bearer_capability(&headers)?,
            unix_time_ms(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn authorize_bootstrap(auth: &BootstrapAuth, headers: &HeaderMap) -> Result<()> {
    match auth {
        BootstrapAuth::LoopbackOnly => Ok(()),
        BootstrapAuth::TokenHash(expected) => {
            let capability = bearer_capability(headers)?;
            let presented = bootstrap_hash(capability.expose());
            if constant_time_equal(expected, &presented) {
                Ok(())
            } else {
                Err(RelayError::Unauthorized)
            }
        }
    }
}

fn bearer_capability(headers: &HeaderMap) -> Result<PresentedCapability> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(RelayError::Unauthorized)?;
    PresentedCapability::parse(value.to_owned())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

const fn default_page_limit() -> u32 {
    100
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{RelayStore, StoreLimits, TokenCipher};

    async fn fixture() -> Router {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.keep().join("relay.sqlite3");
        let metrics = Arc::new(RelayMetrics::default());
        let store = RelayStore::open(
            database,
            TokenCipher::from_key([5; 32]),
            StoreLimits::default(),
            metrics.clone(),
        )
        .unwrap();
        build_router(ApiState {
            backend: RelayBackend::LocalSqlite(Arc::new(store)),
            metrics,
            bootstrap_auth: BootstrapAuth::LoopbackOnly,
            max_body_bytes: 2 * 1_024 * 1_024 + 4_096,
        })
    }

    #[tokio::test]
    async fn installation_capabilities_are_issued_once_and_scoped() {
        let app = fixture().await;
        let response = app
            .oneshot(
                Request::post("/v1/installations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 64 * 1_024).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            value["installation_id"]
                .as_str()
                .unwrap()
                .starts_with("inst_")
        );
        assert!(value["write_capability"].as_str().unwrap().len() >= 32);
        assert_ne!(value["write_capability"], value["read_capability"]);
        assert_ne!(value["read_capability"], value["manage_capability"]);
    }

    #[tokio::test]
    async fn bootstrap_token_failure_is_non_enumerating() {
        let auth = BootstrapAuth::TokenHash(bootstrap_hash("correct-token-that-is-long-enough"));
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            "Bearer wrong-token-that-is-long-enough".parse().unwrap(),
        );
        assert!(matches!(
            authorize_bootstrap(&auth, &headers),
            Err(RelayError::Unauthorized)
        ));
        headers.insert(
            AUTHORIZATION,
            "Bearer correct-token-that-is-long-enough".parse().unwrap(),
        );
        assert!(authorize_bootstrap(&auth, &headers).is_ok());
    }

    #[tokio::test]
    async fn json_boundaries_reject_unknown_fields_without_echoing_values() {
        let app = fixture().await;
        let installation_response = app
            .clone()
            .oneshot(
                Request::post("/v1/installations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(installation_response.into_body(), 64 * 1_024)
            .await
            .unwrap();
        let installation: Value = serde_json::from_slice(&body).unwrap();
        let installation_id = installation["installation_id"].as_str().unwrap();
        let write = installation["write_capability"].as_str().unwrap();
        let secret_canary = "prompt-secret-must-not-be-reflected";
        let request_body = serde_json::json!({
            "event_id": "evt_api_closed_contract_0001",
            "event_class": "state_changed",
            "expires_at_ms": unix_time_ms() + 60_000,
            "ciphertext": "MDAwMDAwMDAwMDAwMDAwMA",
            "prompt": secret_canary
        });
        let response = app
            .oneshot(
                Request::post(format!("/v1/installations/{installation_id}/events"))
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {write}"))
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 64 * 1_024).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"error":"invalid_request"}"#);
        assert!(!String::from_utf8_lossy(&body).contains(secret_canary));
    }

    #[tokio::test]
    async fn device_registration_requires_manage_capability() {
        let app = fixture().await;
        let installation_response = app
            .clone()
            .oneshot(
                Request::post("/v1/installations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(installation_response.into_body(), 64 * 1_024)
            .await
            .unwrap();
        let installation: Value = serde_json::from_slice(&body).unwrap();
        let installation_id = installation["installation_id"].as_str().unwrap();
        let read = installation["read_capability"].as_str().unwrap();
        let manage = installation["manage_capability"].as_str().unwrap();
        let request_body = serde_json::json!({
            "provider": "apns",
            "environment": "sandbox",
            "token": "apns-api-scope-test-token"
        });

        let unauthorized = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/installations/{installation_id}/devices"))
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {read}"))
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/installations/{installation_id}/devices"))
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {manage}"))
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::CREATED);

        let replayed = app
            .oneshot(
                Request::post(format!("/v1/installations/{installation_id}/devices"))
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {manage}"))
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replayed.status(), StatusCode::OK);
    }
}
