use std::sync::{Arc, RwLock};

use futures::future::BoxFuture;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER,
    USER_AGENT,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use crate::config::{
    ANTHROPIC_VERSION, CCR_BYOC_BETA, DEFAULT_RUNNER_VERSION, DEFAULT_USER_AGENT,
    ENVIRONMENTS_BETA, MANAGED_AGENTS_BETA,
};
use crate::error::{BridgeApiError, RemoteControlError};
use crate::wire::{
    BridgeEnvironmentRegistration, BridgeEnvironmentRegistrationResponse, GroveAccountSettings,
    ReconnectSessionRequest, ReconnectSessionResponse, RemoteEvent, SessionCreateRequest,
    SessionEventsPage, SessionEventsPostRequest, SessionRecord, SessionsPage, Work,
    WorkHeartbeatResponse, WorkStopRequest, WorkerDeliveryAckRequest, WorkerEventsPage,
    WorkerEventsRequest, WorkerHeartbeatRequest, WorkerInitRequest, WorkerRegisterResponse,
    WorkerStateResponse,
};

const ANTHROPIC_BETA: HeaderName = HeaderName::from_static("anthropic-beta");
const X_API_KEY: HeaderName = HeaderName::from_static("x-api-key");
const X_ORGANIZATION_UUID: HeaderName = HeaderName::from_static("x-organization-uuid");
const X_TRUSTED_DEVICE_TOKEN: HeaderName = HeaderName::from_static("x-trusted-device-token");
const ANTHROPIC_VERSION_HEADER: HeaderName = HeaderName::from_static("anthropic-version");
const X_ENVIRONMENT_RUNNER_VERSION: HeaderName =
    HeaderName::from_static("x-environment-runner-version");
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeCredential {
    Bearer(String),
    ApiKey(String),
}

impl BridgeCredential {
    pub fn token(&self) -> &str {
        match self {
            Self::Bearer(token) | Self::ApiKey(token) => token,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAuth {
    pub credential: BridgeCredential,
    pub organization_uuid: String,
    pub trusted_device_token: Option<String>,
}

impl BridgeAuth {
    pub fn bearer(access_token: impl Into<String>, organization_uuid: impl Into<String>) -> Self {
        Self {
            credential: BridgeCredential::Bearer(access_token.into()),
            organization_uuid: organization_uuid.into(),
            trusted_device_token: None,
        }
    }

    pub fn api_key(api_key: impl Into<String>, organization_uuid: impl Into<String>) -> Self {
        Self {
            credential: BridgeCredential::ApiKey(api_key.into()),
            organization_uuid: organization_uuid.into(),
            trusted_device_token: None,
        }
    }

    pub fn bearer_token(&self) -> Option<&str> {
        match &self.credential {
            BridgeCredential::Bearer(token) => Some(token),
            BridgeCredential::ApiKey(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBeta {
    Environments,
    RemoteControl,
    ManagedAgents,
    RemoteControlAndManagedAgents,
}

pub type AuthRefreshCallback = Arc<
    dyn Fn(String) -> BoxFuture<'static, Result<Option<BridgeAuth>, RemoteControlError>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct BridgeApiClient {
    http: reqwest::Client,
    base_url: Url,
    auth: Arc<RwLock<BridgeAuth>>,
    user_agent: String,
    runner_version: String,
    auth_refresh: Option<AuthRefreshCallback>,
}

#[derive(Clone)]
pub struct BridgeApiClientBuilder {
    http: Option<reqwest::Client>,
    base_url: Url,
    auth: BridgeAuth,
    user_agent: String,
    runner_version: String,
    auth_refresh: Option<AuthRefreshCallback>,
}

impl BridgeApiClientBuilder {
    pub fn new(base_url: Url, auth: BridgeAuth) -> Self {
        Self {
            http: None,
            base_url,
            auth,
            user_agent: DEFAULT_USER_AGENT.to_string(),
            runner_version: DEFAULT_RUNNER_VERSION.to_string(),
            auth_refresh: None,
        }
    }

    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn runner_version(mut self, runner_version: impl Into<String>) -> Self {
        self.runner_version = runner_version.into();
        self
    }

    pub fn trusted_device_token(mut self, token: impl Into<String>) -> Self {
        self.auth.trusted_device_token = Some(token.into());
        self
    }

    pub fn auth_refresh_callback(mut self, callback: AuthRefreshCallback) -> Self {
        self.auth_refresh = Some(callback);
        self
    }

    pub fn build(self) -> BridgeApiClient {
        BridgeApiClient {
            http: self.http.unwrap_or_default(),
            base_url: self.base_url,
            auth: Arc::new(RwLock::new(self.auth)),
            user_agent: self.user_agent,
            runner_version: self.runner_version,
            auth_refresh: self.auth_refresh,
        }
    }
}

impl BridgeApiClient {
    pub fn builder(base_url: Url, auth: BridgeAuth) -> BridgeApiClientBuilder {
        BridgeApiClientBuilder::new(base_url, auth)
    }

    pub fn new(base_url: Url, auth: BridgeAuth) -> Self {
        Self::builder(base_url, auth).build()
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub fn headers(&self, beta: RequestBeta) -> Result<HeaderMap, RemoteControlError> {
        let auth = self
            .auth
            .read()
            .map_err(|_| RemoteControlError::Protocol("bridge auth lock poisoned".to_string()))?
            .clone();
        self.headers_for_auth(&auth, beta)
    }

    pub fn worker_headers(
        &self,
        session_ingress_token: &str,
    ) -> Result<HeaderMap, RemoteControlError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {session_ingress_token}")).map_err(|err| {
                RemoteControlError::Protocol(format!("invalid worker authorization header: {err}"))
            })?,
        );
        headers.insert(
            ANTHROPIC_VERSION_HEADER,
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent).map_err(|err| {
                RemoteControlError::Protocol(format!("invalid user-agent header: {err}"))
            })?,
        );
        Ok(headers)
    }

    pub fn oauth_headers(&self) -> Result<HeaderMap, RemoteControlError> {
        let auth = self
            .auth
            .read()
            .map_err(|_| RemoteControlError::Protocol("bridge auth lock poisoned".to_string()))?
            .clone();
        let mut headers = HeaderMap::new();
        match &auth.credential {
            BridgeCredential::Bearer(token) => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                        RemoteControlError::Protocol(format!(
                            "invalid oauth authorization header: {err}"
                        ))
                    })?,
                );
            }
            BridgeCredential::ApiKey(key) => {
                headers.insert(
                    X_API_KEY,
                    HeaderValue::from_str(key).map_err(|err| {
                        RemoteControlError::Protocol(format!("invalid x-api-key header: {err}"))
                    })?,
                );
            }
        }
        headers.insert(ANTHROPIC_BETA, HeaderValue::from_static(OAUTH_BETA));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent).map_err(|err| {
                RemoteControlError::Protocol(format!("invalid user-agent header: {err}"))
            })?,
        );
        Ok(headers)
    }

    fn headers_for_auth(
        &self,
        auth: &BridgeAuth,
        beta: RequestBeta,
    ) -> Result<HeaderMap, RemoteControlError> {
        let mut headers = HeaderMap::new();
        match &auth.credential {
            BridgeCredential::Bearer(token) => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                        RemoteControlError::Protocol(format!("invalid authorization header: {err}"))
                    })?,
                );
            }
            BridgeCredential::ApiKey(key) => {
                headers.insert(
                    X_API_KEY,
                    HeaderValue::from_str(key).map_err(|err| {
                        RemoteControlError::Protocol(format!("invalid x-api-key header: {err}"))
                    })?,
                );
            }
        }
        headers.insert(
            X_ORGANIZATION_UUID,
            HeaderValue::from_str(&auth.organization_uuid).map_err(|err| {
                RemoteControlError::Protocol(format!("invalid organization header: {err}"))
            })?,
        );
        headers.insert(
            ANTHROPIC_VERSION_HEADER,
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent).map_err(|err| {
                RemoteControlError::Protocol(format!("invalid user-agent header: {err}"))
            })?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ANTHROPIC_BETA,
            HeaderValue::from_static(match beta {
                RequestBeta::Environments => ENVIRONMENTS_BETA,
                RequestBeta::RemoteControl => CCR_BYOC_BETA,
                RequestBeta::ManagedAgents => MANAGED_AGENTS_BETA,
                RequestBeta::RemoteControlAndManagedAgents => {
                    "ccr-byoc-2025-07-29,managed-agents-2026-04-01"
                }
            }),
        );
        if matches!(beta, RequestBeta::Environments) {
            headers.insert(
                X_ENVIRONMENT_RUNNER_VERSION,
                HeaderValue::from_str(&self.runner_version).map_err(|err| {
                    RemoteControlError::Protocol(format!(
                        "invalid environment runner version header: {err}"
                    ))
                })?,
            );
        }
        if let Some(token) = &auth.trusted_device_token {
            headers.insert(
                X_TRUSTED_DEVICE_TOKEN,
                HeaderValue::from_str(token).map_err(|err| {
                    RemoteControlError::Protocol(format!("invalid trusted-device token: {err}"))
                })?,
            );
        }
        Ok(headers)
    }

    pub async fn register_environment(
        &self,
        body: &BridgeEnvironmentRegistration,
    ) -> Result<BridgeEnvironmentRegistrationResponse, RemoteControlError> {
        self.post_json("/v1/environments/bridge", body, RequestBeta::Environments)
            .await
    }

    pub async fn deregister_environment(
        &self,
        environment_id: &str,
    ) -> Result<(), RemoteControlError> {
        self.request_empty(
            Method::DELETE,
            &format!("/v1/environments/bridge/{environment_id}"),
            Option::<&()>::None,
            RequestBeta::Environments,
        )
        .await
    }

    pub async fn poll_for_work(
        &self,
        environment_id: &str,
        reclaim_older_than_ms: Option<u64>,
    ) -> Result<Option<Work>, RemoteControlError> {
        let mut path = format!("/v1/environments/{environment_id}/work/poll");
        if let Some(ms) = reclaim_older_than_ms {
            path.push_str(&format!("?reclaim_older_than_ms={ms}"));
        }
        self.get_json(&path, RequestBeta::Environments).await
    }

    pub async fn acknowledge_work(
        &self,
        environment_id: &str,
        work_id: &str,
    ) -> Result<(), RemoteControlError> {
        self.request_empty(
            Method::POST,
            &format!("/v1/environments/{environment_id}/work/{work_id}/ack"),
            Some(&json!({})),
            RequestBeta::Environments,
        )
        .await
    }

    pub async fn heartbeat_work(
        &self,
        environment_id: &str,
        work_id: &str,
    ) -> Result<WorkHeartbeatResponse, RemoteControlError> {
        self.request_json(
            Method::POST,
            &format!("/v1/environments/{environment_id}/work/{work_id}/heartbeat"),
            Some(&json!({})),
            RequestBeta::Environments,
        )
        .await
    }

    pub async fn stop_work(
        &self,
        environment_id: &str,
        work_id: &str,
        force: bool,
    ) -> Result<(), RemoteControlError> {
        self.request_empty(
            Method::POST,
            &format!("/v1/environments/{environment_id}/work/{work_id}/stop"),
            Some(&WorkStopRequest { force }),
            RequestBeta::Environments,
        )
        .await
    }

    pub async fn reconnect_session(
        &self,
        environment_id: &str,
        session_id: &str,
    ) -> Result<ReconnectSessionResponse, RemoteControlError> {
        self.post_json(
            &format!("/v1/environments/{environment_id}/bridge/reconnect"),
            &ReconnectSessionRequest {
                session_id: session_id.to_string(),
            },
            RequestBeta::Environments,
        )
        .await
    }

    pub async fn archive_session(&self, session_id: &str) -> Result<(), RemoteControlError> {
        self.request_empty(
            Method::POST,
            &format!("/v1/sessions/{session_id}/archive?beta=true"),
            Some(&json!({})),
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn create_session(
        &self,
        body: &SessionCreateRequest,
    ) -> Result<SessionRecord, RemoteControlError> {
        self.post_json(
            "/v1/sessions?beta=true",
            body,
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn list_sessions(
        &self,
        page: Option<&str>,
        limit: Option<usize>,
    ) -> Result<SessionsPage, RemoteControlError> {
        let suffix = beta_query([
            page.map(|page| ("page", page.to_string())),
            limit.map(|limit| ("limit", limit.to_string())),
        ]);
        self.get_json(
            &format!("/v1/sessions{suffix}"),
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn fetch_session(
        &self,
        session_id: &str,
    ) -> Result<SessionRecord, RemoteControlError> {
        self.get_json(
            &format!("/v1/sessions/{session_id}?beta=true"),
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn update_session<B: Serialize + ?Sized>(
        &self,
        session_id: &str,
        body: &B,
    ) -> Result<SessionRecord, RemoteControlError> {
        self.post_json(
            &format!("/v1/sessions/{session_id}?beta=true"),
            body,
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), RemoteControlError> {
        self.request_empty::<()>(
            Method::DELETE,
            &format!("/v1/sessions/{session_id}?beta=true"),
            None,
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn post_session_events(
        &self,
        session_id: &str,
        events: Vec<RemoteEvent>,
    ) -> Result<(), RemoteControlError> {
        self.request_empty(
            Method::POST,
            &format!("/v1/sessions/{session_id}/events?beta=true"),
            Some(&SessionEventsPostRequest { events }),
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn get_session_events(
        &self,
        session_id: &str,
        page: Option<&str>,
        limit: Option<usize>,
    ) -> Result<SessionEventsPage, RemoteControlError> {
        let suffix = beta_query([
            page.map(|page| ("page", page.to_string())),
            limit.map(|limit| ("limit", limit.to_string())),
        ]);
        self.get_json(
            &format!("/v1/sessions/{session_id}/events{suffix}"),
            RequestBeta::RemoteControlAndManagedAgents,
        )
        .await
    }

    pub async fn session_events_stream_response(
        &self,
        session_id: &str,
        last_event_id: Option<&str>,
        from_sequence_num: Option<u64>,
    ) -> Result<reqwest::Response, RemoteControlError> {
        let suffix =
            beta_query([from_sequence_num.map(|seq| ("from_sequence_num", seq.to_string()))]);
        let url = self.url(&format!("/v1/sessions/{session_id}/events/stream{suffix}"))?;
        let mut req = self
            .http
            .get(url)
            .headers(self.headers(RequestBeta::RemoteControlAndManagedAgents)?)
            .header(ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            req = req.header("Last-Event-ID", last_event_id);
        }
        let resp = req.send().await?;
        self.error_for_status(resp).await
    }

    pub async fn worker_register(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
    ) -> Result<WorkerRegisterResponse, RemoteControlError> {
        let url = Url::parse(session_base_url)?.join("worker/register")?;
        self.request_json_url_with_headers(
            Method::POST,
            url,
            Some(&json!({})),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_init(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        body: &WorkerInitRequest,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::PUT,
            Url::parse(session_base_url)?.join("worker")?,
            Some(body),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_state(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
    ) -> Result<WorkerStateResponse, RemoteControlError> {
        self.request_json_url_with_headers::<(), WorkerStateResponse>(
            Method::GET,
            Url::parse(session_base_url)?.join("worker")?,
            None,
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_events_stream_response(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response, RemoteControlError> {
        let url = Url::parse(session_base_url)?.join("worker/events/stream")?;
        let mut headers = self.worker_headers(session_ingress_token)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let mut req = self.http.get(url).headers(headers);
        if let Some(last_event_id) = last_event_id {
            req = req.header("Last-Event-ID", last_event_id);
        }
        self.error_for_status(req.send().await?).await
    }

    pub async fn worker_heartbeat(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        body: &WorkerHeartbeatRequest,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::POST,
            Url::parse(session_base_url)?.join("worker/heartbeat")?,
            Some(body),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_events(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        body: &WorkerEventsRequest,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::POST,
            Url::parse(session_base_url)?.join("worker/events")?,
            Some(body),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_internal_events(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        body: &WorkerEventsRequest,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::POST,
            Url::parse(session_base_url)?.join("worker/internal-events")?,
            Some(body),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn get_worker_internal_events(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        subagents: bool,
    ) -> Result<WorkerEventsPage, RemoteControlError> {
        let suffix = if subagents { "?subagents=true" } else { "" };
        let url = Url::parse(session_base_url)?.join(&format!("worker/internal-events{suffix}"))?;
        self.request_json_url_with_headers::<(), WorkerEventsPage>(
            Method::GET,
            url,
            None,
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn worker_delivery_ack(
        &self,
        session_base_url: &str,
        session_ingress_token: &str,
        body: &WorkerDeliveryAckRequest,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::POST,
            Url::parse(session_base_url)?.join("worker/events/delivery")?,
            Some(body),
            self.worker_headers(session_ingress_token)?,
        )
        .await
    }

    pub async fn mark_grove_notice_viewed(&self) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::POST,
            self.url("/api/oauth/account/grove_notice_viewed")?,
            Some(&json!({})),
            self.oauth_headers()?,
        )
        .await
    }

    pub async fn get_grove_account_settings(
        &self,
    ) -> Result<GroveAccountSettings, RemoteControlError> {
        self.request_json_url_with_headers::<(), GroveAccountSettings>(
            Method::GET,
            self.url("/api/oauth/account/settings")?,
            None,
            self.oauth_headers()?,
        )
        .await
    }

    pub async fn update_grove_enabled(&self, enabled: bool) -> Result<(), RemoteControlError> {
        self.request_empty_url_with_headers(
            Method::PATCH,
            self.url("/api/oauth/account/settings")?,
            Some(&json!({ "grove_enabled": enabled })),
            self.oauth_headers()?,
        )
        .await
    }

    pub async fn raw_json_path<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<T, RemoteControlError> {
        self.request_json(method, path, body, beta).await
    }

    pub fn url(&self, path: &str) -> Result<Url, RemoteControlError> {
        Ok(self.base_url.join(path.trim_start_matches('/'))?)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        beta: RequestBeta,
    ) -> Result<T, RemoteControlError> {
        self.request_json::<(), T>(Method::GET, path, None, beta)
            .await
    }

    async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        beta: RequestBeta,
    ) -> Result<T, RemoteControlError> {
        self.request_json(Method::POST, path, Some(body), beta)
            .await
    }

    async fn request_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<T, RemoteControlError> {
        self.request_json_url(method, self.url(path)?, body, beta)
            .await
    }

    async fn request_json_url<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<T, RemoteControlError> {
        let resp = self.request_raw(method, url, body, beta).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(serde_json::from_value(Value::Null)?);
        }
        Ok(resp.json::<T>().await?)
    }

    async fn request_empty<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<(), RemoteControlError> {
        self.request_empty_url(method, self.url(path)?, body, beta)
            .await
    }

    async fn request_empty_url<B: Serialize + ?Sized>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<(), RemoteControlError> {
        let _ = self.request_raw(method, url, body, beta).await?;
        Ok(())
    }

    async fn request_json_url_with_headers<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        headers: HeaderMap,
    ) -> Result<T, RemoteControlError> {
        let resp = self
            .request_raw_with_headers(method, url, body, headers)
            .await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(serde_json::from_value(Value::Null)?);
        }
        Ok(resp.json::<T>().await?)
    }

    async fn request_empty_url_with_headers<B: Serialize + ?Sized>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        headers: HeaderMap,
    ) -> Result<(), RemoteControlError> {
        let _ = self
            .request_raw_with_headers(method, url, body, headers)
            .await?;
        Ok(())
    }

    async fn request_raw<B: Serialize + ?Sized>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        beta: RequestBeta,
    ) -> Result<reqwest::Response, RemoteControlError> {
        let auth = self
            .auth
            .read()
            .map_err(|_| RemoteControlError::Protocol("bridge auth lock poisoned".to_string()))?
            .clone();
        let mut req = self
            .http
            .request(method.clone(), url.clone())
            .headers(self.headers_for_auth(&auth, beta)?);
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        if resp.status() == StatusCode::UNAUTHORIZED
            && let (Some(callback), Some(old_token)) =
                (self.auth_refresh.as_ref(), auth.bearer_token())
            && let Some(new_auth) = callback(old_token.to_string()).await?
        {
            self.auth
                .write()
                .map_err(|_| RemoteControlError::Protocol("bridge auth lock poisoned".to_string()))?
                .clone_from(&new_auth);
            let mut retry_req = self
                .http
                .request(method, url)
                .headers(self.headers_for_auth(&new_auth, beta)?);
            if let Some(body) = body {
                retry_req = retry_req.json(body);
            }
            return self.error_for_status(retry_req.send().await?).await;
        }
        self.error_for_status(resp).await
    }

    async fn request_raw_with_headers<B: Serialize + ?Sized>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
        headers: HeaderMap,
    ) -> Result<reqwest::Response, RemoteControlError> {
        let mut req = self.http.request(method, url).headers(headers);
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        self.error_for_status(resp).await
    }

    async fn error_for_status(
        &self,
        resp: reqwest::Response,
    ) -> Result<reqwest::Response, RemoteControlError> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let headers = resp.headers().clone();
        let cookie = headers
            .get("cookie")
            .or_else(|| headers.get("set-cookie"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let retry_after = headers
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.unwrap_or_default();
        Err(BridgeApiError::from_response_parts(
            status,
            cookie.as_deref(),
            retry_after.as_deref(),
            body,
        )
        .into())
    }
}

fn beta_query<const N: usize>(parts: [Option<(&str, String)>; N]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("beta", "true");
    for (key, value) in parts.into_iter().flatten() {
        serializer.append_pair(key, &value);
    }
    format!("?{}", serializer.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_headers_include_claude_remote_control_contract() {
        let client = BridgeApiClient::new(
            Url::parse("https://api.anthropic.com").unwrap(),
            BridgeAuth {
                credential: BridgeCredential::Bearer("tok".to_string()),
                organization_uuid: "org".to_string(),
                trusted_device_token: Some("device".to_string()),
            },
        );
        let headers = client.headers(RequestBeta::Environments).unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer tok");
        assert_eq!(headers[X_ORGANIZATION_UUID], "org");
        assert_eq!(headers[ANTHROPIC_VERSION_HEADER], ANTHROPIC_VERSION);
        assert_eq!(headers[ANTHROPIC_BETA], ENVIRONMENTS_BETA);
        assert_eq!(
            headers[X_ENVIRONMENT_RUNNER_VERSION],
            DEFAULT_RUNNER_VERSION
        );
        assert_eq!(headers[X_TRUSTED_DEVICE_TOKEN], "device");
    }

    #[test]
    fn worker_headers_use_session_ingress_token_only() {
        let client = BridgeApiClient::new(
            Url::parse("https://api.anthropic.com").unwrap(),
            BridgeAuth::bearer("oauth", "org"),
        );
        let headers = client.worker_headers("session-token").unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer session-token");
        assert_eq!(headers[ANTHROPIC_VERSION_HEADER], ANTHROPIC_VERSION);
        assert!(headers.get(ANTHROPIC_BETA).is_none());
        assert!(headers.get(X_ORGANIZATION_UUID).is_none());
    }

    #[test]
    fn url_joins_root_paths() {
        let client = BridgeApiClient::new(
            Url::parse("https://api.anthropic.com").unwrap(),
            BridgeAuth::bearer("tok", "org"),
        );
        assert_eq!(
            client.url("/v1/environments/bridge").unwrap().as_str(),
            "https://api.anthropic.com/v1/environments/bridge"
        );
    }

    #[test]
    fn managed_agents_page_query_uses_page_not_cursor() {
        assert_eq!(
            beta_query([
                Some(("page", "page/with spaces".to_string())),
                Some(("limit", "25".to_string()))
            ]),
            "?beta=true&page=page%2Fwith+spaces&limit=25"
        );
    }
}
