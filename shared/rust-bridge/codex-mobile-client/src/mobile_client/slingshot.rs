use super::*;

const SLINGSHOT_CREDENTIALS_DIR_NAME: &str = "slingshot";
const SLINGSHOT_CREDENTIALS_VERSION: u32 = 1;
const SLINGSHOT_TOKEN_REFRESH_SKEW_SECS: i64 = 30;
const SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_ATTEMPTS: usize = 3;
const SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_DELAY_SECS: u64 = 5;

pub(crate) fn slingshot_user_agent() -> String {
    let arch = slingshot_user_agent_arch();
    if cfg!(target_os = "android") {
        format!("Codex Desktop/26.513.20950 (Android; {arch})")
    } else if cfg!(target_os = "ios") {
        format!("Codex Desktop/26.513.20950 (iOS; {arch})")
    } else {
        format!("Codex Desktop/26.513.20950 (Macintosh; Intel Mac OS X; {arch})")
    }
}

fn slingshot_user_agent_arch() -> &'static str {
    if cfg!(all(target_os = "android", target_arch = "aarch64")) {
        "arm64-v8a"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

fn slingshot_api_cache_key(base_url: &Url, account_id: &str) -> String {
    format!("{}|{}", base_url.as_str().trim_end_matches('/'), account_id)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSlingshotControllerSession {
    version: u32,
    base_url: String,
    account_id: String,
    session: codex_slingshot::SlingshotControllerSession,
}

fn slingshot_credentials_path(root: &Path, base_url: &Url, account_id: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    base_url.as_str().trim_end_matches('/').hash(&mut hasher);
    account_id.hash(&mut hasher);
    root.join(SLINGSHOT_CREDENTIALS_DIR_NAME)
        .join(format!("{:016x}.json", hasher.finish()))
}

fn slingshot_session_is_usable(session: &codex_slingshot::SlingshotControllerSession) -> bool {
    let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&session.expires_at) else {
        return false;
    };
    let expires_at = expires_at.with_timezone(&chrono::Utc);
    expires_at > chrono::Utc::now() + chrono::Duration::seconds(SLINGSHOT_TOKEN_REFRESH_SKEW_SECS)
}

pub(super) fn is_slingshot_initialize_timeout(error: &TransportError) -> bool {
    match error {
        TransportError::ConnectionFailed(message) => {
            message.contains("slingshot app-server handshake failed")
                && message.contains("timed out waiting for initialize response")
        }
        _ => false,
    }
}

async fn connect_slingshot_with_startup_retries(
    api: codex_slingshot::SlingshotApi,
    environment_id: String,
    args: &codex_app_server_client::RemoteAppServerConnectArgs,
    server_id: &str,
) -> Result<codex_app_server_client::AppServerClient, TransportError> {
    for attempt in 1..=SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_ATTEMPTS {
        match connect_remote_client_over_slingshot(api.clone(), environment_id.clone(), args).await
        {
            Ok(client) => {
                if attempt > 1 {
                    info!(
                        target: "codex_slingshot",
                        %server_id,
                        %environment_id,
                        attempt,
                        "Slingshot app-server initialize succeeded after retry"
                    );
                }
                return Ok(client);
            }
            Err(error)
                if attempt < SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_ATTEMPTS
                    && is_slingshot_initialize_timeout(&error) =>
            {
                warn!(
                    target: "codex_slingshot",
                    %server_id,
                    %environment_id,
                    attempt,
                    max_attempts = SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_ATTEMPTS,
                    retry_delay_secs = SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_DELAY_SECS,
                    %error,
                    "Slingshot app-server initialize timed out; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(
                    SLINGSHOT_INITIALIZE_TIMEOUT_RETRY_DELAY_SECS,
                ))
                .await;
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("Slingshot retry loop always returns from the final attempt")
}

impl MobileClient {
    fn slingshot_credentials_directory(&self) -> Option<PathBuf> {
        match self.slingshot_credentials_directory.lock() {
            Ok(guard) => guard.clone().map(PathBuf::from),
            Err(error) => error.into_inner().clone().map(PathBuf::from),
        }
    }

    fn load_persisted_slingshot_session(
        &self,
        base_url: &Url,
        account_id: &str,
    ) -> Option<codex_slingshot::SlingshotControllerSession> {
        let root = self.slingshot_credentials_directory()?;
        let path = slingshot_credentials_path(&root, base_url, account_id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                warn!(
                    target: "codex_slingshot",
                    path = %path.display(),
                    %error,
                    "failed to read persisted Slingshot controller session"
                );
                return None;
            }
        };
        let stored: StoredSlingshotControllerSession = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(error) => {
                warn!(
                    target: "codex_slingshot",
                    path = %path.display(),
                    %error,
                    "failed to decode persisted Slingshot controller session"
                );
                return None;
            }
        };
        if stored.version != SLINGSHOT_CREDENTIALS_VERSION
            || stored.base_url != base_url.as_str().trim_end_matches('/')
            || stored.account_id != account_id
        {
            warn!(
                target: "codex_slingshot",
                path = %path.display(),
                version = stored.version,
                "ignoring mismatched persisted Slingshot controller session"
            );
            return None;
        }
        if slingshot_session_is_usable(&stored.session) {
            info!(
                target: "codex_slingshot",
                path = %path.display(),
                client_id = %stored.session.client_id,
                account_user_id = %stored.session.account_user_id,
                expires_at = %stored.session.expires_at,
                "loaded persisted Slingshot controller session"
            );
        } else {
            info!(
                target: "codex_slingshot",
                path = %path.display(),
                client_id = %stored.session.client_id,
                account_user_id = %stored.session.account_user_id,
                expires_at = %stored.session.expires_at,
                "loaded expired persisted Slingshot controller session"
            );
        }
        Some(stored.session)
    }

    fn persist_slingshot_session(
        &self,
        base_url: &Url,
        account_id: &str,
        session: &codex_slingshot::SlingshotControllerSession,
    ) {
        let Some(root) = self.slingshot_credentials_directory() else {
            warn!(
                target: "codex_slingshot",
                "Slingshot controller session persistence skipped because directory is unset"
            );
            return;
        };
        let path = slingshot_credentials_path(&root, base_url, account_id);
        let stored = StoredSlingshotControllerSession {
            version: SLINGSHOT_CREDENTIALS_VERSION,
            base_url: base_url.as_str().trim_end_matches('/').to_string(),
            account_id: account_id.to_string(),
            session: session.clone(),
        };
        let result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec_pretty(&stored)?;
            std::fs::write(&path, bytes)?;
            Ok(())
        })();
        match result {
            Ok(()) => info!(
                target: "codex_slingshot",
                path = %path.display(),
                client_id = %session.client_id,
                account_user_id = %session.account_user_id,
                expires_at = %session.expires_at,
                "persisted Slingshot controller session"
            ),
            Err(error) => warn!(
                target: "codex_slingshot",
                path = %path.display(),
                %error,
                "failed to persist Slingshot controller session"
            ),
        }
    }

    pub async fn connect_remote_over_slingshot(
        &self,
        server_id: String,
        display_name: String,
        base_url: String,
        access_token: String,
        account_id: String,
        environment_id: String,
        step_up_token: String,
    ) -> Result<String, TransportError> {
        if self.existing_active_session(server_id.as_str()).is_some() {
            info!("MobileClient: reusing existing Slingshot server session {server_id}");
            return Ok(server_id);
        }

        let base_url = Url::parse(base_url.trim()).map_err(|error| {
            TransportError::ConnectionFailed(format!("invalid Slingshot base URL: {error}"))
        })?;
        let access_token = access_token.trim().to_string();
        if access_token.is_empty() {
            return Err(TransportError::ConnectionFailed(
                "missing ChatGPT access token for Slingshot".to_string(),
            ));
        }
        let account_id = account_id.trim().to_string();
        if account_id.is_empty() {
            return Err(TransportError::ConnectionFailed(
                "missing ChatGPT account id for Slingshot enrollment".to_string(),
            ));
        }
        let environment_id = environment_id.trim().to_string();
        if environment_id.is_empty() {
            return Err(TransportError::ConnectionFailed(
                "missing Slingshot environment id".to_string(),
            ));
        }
        let step_up_token = step_up_token.trim().to_string();
        let cache_key = slingshot_api_cache_key(&base_url, &account_id);
        let cached_api = match self.slingshot_apis.lock() {
            Ok(guard) => guard.get(&cache_key).cloned(),
            Err(error) => error.into_inner().get(&cache_key).cloned(),
        }
        .and_then(|api| {
            if api
                .controller_session()
                .as_ref()
                .is_some_and(slingshot_session_is_usable)
            {
                Some(api)
            } else {
                info!(
                    target: "codex_slingshot",
                    %server_id,
                    %environment_id,
                    "MobileClient: cached Slingshot enrollment missing or expired"
                );
                None
            }
        });
        let api = if step_up_token.is_empty() {
            if let Some(api) = cached_api {
                info!(
                    target: "codex_slingshot",
                    %server_id,
                    %environment_id,
                    has_access_token = true,
                    has_step_up_token = false,
                    "MobileClient: reusing cached Slingshot enrollment"
                );
                api
            } else if let Some(session) =
                self.load_persisted_slingshot_session(&base_url, &account_id)
            {
                let session_is_usable = slingshot_session_is_usable(&session);
                let api = codex_slingshot::SlingshotApi::new(codex_slingshot::SlingshotConfig {
                    base_url: base_url.clone(),
                    auth_token: access_token.clone(),
                    user_agent: slingshot_user_agent(),
                    account_id: Some(account_id.clone()),
                    originator: Some("Codex Desktop".to_string()),
                    client_id: Some(session.client_id.clone()),
                });
                if session_is_usable {
                    api.restore_controller_session(session);
                    info!(
                        target: "codex_slingshot",
                        %server_id,
                        %environment_id,
                        has_access_token = true,
                        has_step_up_token = false,
                        "MobileClient: restored persisted Slingshot enrollment"
                    );
                } else {
                    info!(
                        target: "codex_slingshot",
                        %server_id,
                        %environment_id,
                        client_id = %session.client_id,
                        expires_at = %session.expires_at,
                        has_access_token = true,
                        has_step_up_token = false,
                        "MobileClient: refreshing expired Slingshot enrollment"
                    );
                    api.refresh_with_device_key(&session)
                        .await
                        .map_err(|error| {
                            warn!(
                                target: "codex_slingshot",
                                %server_id,
                                %environment_id,
                                %error,
                                "MobileClient: Slingshot enrollment refresh failed"
                            );
                            TransportError::ConnectionFailed(
                                "missing Slingshot remote-control authorization token".to_string(),
                            )
                        })?;
                    if let Some(session) = api.controller_session() {
                        self.persist_slingshot_session(&base_url, &account_id, &session);
                    }
                    info!(
                        target: "codex_slingshot",
                        %server_id,
                        %environment_id,
                        has_access_token = true,
                        has_step_up_token = false,
                        "MobileClient: refreshed persisted Slingshot enrollment"
                    );
                }
                match self.slingshot_apis.lock() {
                    Ok(mut guard) => {
                        guard.insert(cache_key.clone(), api.clone());
                    }
                    Err(error) => {
                        error.into_inner().insert(cache_key.clone(), api.clone());
                    }
                }
                api
            } else {
                warn!(
                    target: "codex_slingshot",
                    %server_id,
                    %environment_id,
                    has_access_token = true,
                    has_step_up_token = false,
                    "MobileClient: no persisted Slingshot enrollment available"
                );
                return Err(TransportError::ConnectionFailed(
                    "missing Slingshot remote-control authorization token".to_string(),
                ));
            }
        } else {
            let api = codex_slingshot::SlingshotApi::new(codex_slingshot::SlingshotConfig {
                base_url: base_url.clone(),
                auth_token: access_token.clone(),
                user_agent: slingshot_user_agent(),
                account_id: Some(account_id.clone()),
                originator: Some("Codex Desktop".to_string()),
                client_id: None,
            });
            info!(
                target: "codex_slingshot",
                %server_id,
                %environment_id,
                has_access_token = true,
                has_step_up_token = true,
                "MobileClient: starting Slingshot enrollment"
            );
            api.enroll_with_step_up_token(&step_up_token)
                .await
                .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?;
            if let Some(session) = api.controller_session() {
                self.persist_slingshot_session(&base_url, &account_id, &session);
            }
            match self.slingshot_apis.lock() {
                Ok(mut guard) => {
                    guard.insert(cache_key, api.clone());
                }
                Err(error) => {
                    error.into_inner().insert(cache_key, api.clone());
                }
            }
            info!(
                target: "codex_slingshot",
                %server_id,
                %environment_id,
                "MobileClient: Slingshot enrollment complete"
            );
            api
        };

        let config = ServerConfig {
            server_id: server_id.clone(),
            display_name,
            host: environment_id.clone(),
            port: 0,
            websocket_url: build_slingshot_connection_url(&environment_id, base_url.as_str()),
            is_local: false,
            tls: true,
        };
        self.app_store
            .upsert_server(&config, ServerHealthSnapshot::Connecting);
        self.replace_existing_session(server_id.as_str()).await;

        let (_, args) = remote_connect_args(&config);
        let initial_client = connect_slingshot_with_startup_retries(
            api.clone(),
            environment_id.clone(),
            &args,
            &server_id,
        )
        .await
        .inspect_err(|_| {
            self.app_store
                .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
        })?;
        let trait_transport: Arc<dyn crate::session::remote_transport::RemoteTransport> =
            Arc::new(SlingshotReconnectTransport {
                api,
                environment_id: environment_id.clone(),
            });
        let resource = RuntimeRemoteSessionResource {
            runtime_kind: "codex".to_string(),
            client: initial_client,
            transport: Some(trait_transport),
            keepalive: None,
        };
        let session = match ServerSession::connect_remote_multiplexed(
            config,
            vec![resource],
            RemoteSessionExtras::default(),
        )
        .await
        {
            Ok(session) => Arc::new(session),
            Err(error) => {
                self.app_store
                    .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
                return Err(error);
            }
        };
        let runtime_infos = vec![AgentRuntimeInfo {
            kind: "codex".to_string(),
            name: "codex".to_string(),
            display_name: "Codex".to_string(),
            available: true,
        }];
        self.attach_remote_session(&server_id, session, runtime_infos);
        info!("MobileClient: connected Slingshot server {server_id}");
        Ok(server_id)
    }
}
