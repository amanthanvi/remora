use super::*;

impl MobileClient {
    pub async fn connect_remote_over_ssh_bridges(
        &self,
        ssh_client: Arc<SshClient>,
        server_id: String,
        display_name: String,
        host: String,
        state_root: String,
        runtime_kinds: Vec<AgentRuntimeKind>,
        transport: crate::ssh_bridge::SshBridgeTransport,
    ) -> Result<SshBridgeConnectOutcome, TransportError> {
        self.connect_remote_over_ssh_bridges_inner(
            ssh_client,
            server_id,
            display_name,
            host,
            state_root,
            runtime_kinds,
            transport,
            None,
        )
        .await?
        .ok_or_else(|| TransportError::ConnectionFailed("cold reconnect deferred".to_string()))
    }

    pub(crate) async fn reconnect_remote_over_ssh_bridges(
        &self,
        ssh_client: Arc<SshClient>,
        server_id: String,
        display_name: String,
        host: String,
        state_root: String,
        runtime_kinds: Vec<AgentRuntimeKind>,
        transport: crate::ssh_bridge::SshBridgeTransport,
        cold_guard: ColdReconnectGuard,
    ) -> Result<Option<SshBridgeConnectOutcome>, TransportError> {
        self.connect_remote_over_ssh_bridges_inner(
            ssh_client,
            server_id,
            display_name,
            host,
            state_root,
            runtime_kinds,
            transport,
            Some(cold_guard),
        )
        .await
    }

    async fn connect_remote_over_ssh_bridges_inner(
        &self,
        ssh_client: Arc<SshClient>,
        server_id: String,
        display_name: String,
        host: String,
        state_root: String,
        runtime_kinds: Vec<AgentRuntimeKind>,
        transport: crate::ssh_bridge::SshBridgeTransport,
        cold_guard: Option<ColdReconnectGuard>,
    ) -> Result<Option<SshBridgeConnectOutcome>, TransportError> {
        if runtime_kinds.is_empty() {
            return Err(TransportError::ConnectionFailed(
                "no SSH runtime kinds selected".to_string(),
            ));
        }

        let visible_server_id = format!("ssh-bridge:{host}");
        let server_id = if server_id.starts_with(&visible_server_id) {
            visible_server_id
        } else {
            server_id
        };
        let config = ServerConfig {
            server_id: server_id.clone(),
            display_name,
            host: host.clone(),
            port: 0,
            websocket_url: Some(format!("ssh-bridge://{host}")),
            is_local: false,
            tls: false,
        };
        if !self
            .replace_existing_session_with_guard(server_id.as_str(), cold_guard.as_ref())
            .await
        {
            return Ok(None);
        }
        self.app_store
            .upsert_server(&config, ServerHealthSnapshot::Connecting);

        let (runtime_resources, runtime_infos) =
            crate::ssh_bridge::connect_runtime_resources_via_ssh(
                ssh_client,
                state_root,
                runtime_kinds,
                transport,
                host.contains(':'),
            )
            .await
            .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?;
        info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: SSH bridge runtime resources ready server_id={} runtimes={:?} infos={:?}",
            server_id,
            runtime_resources
                .iter()
                .map(|resource| resource.runtime_kind.clone())
                .collect::<Vec<_>>(),
            runtime_infos
        );
        if runtime_resources.is_empty() {
            self.app_store
                .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
            return Err(TransportError::ConnectionFailed(
                "no available SSH bridge runtime streams connected".to_string(),
            ));
        }

        let session = match ServerSession::connect_remote_multiplexed(
            config,
            runtime_resources,
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
        info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: SSH bridge session ready server_id={} runtime_kinds={:?}",
            server_id,
            session.runtime_kinds()
        );
        self.attach_remote_session(&server_id, session, runtime_infos.clone());

        Ok(Some(SshBridgeConnectOutcome {
            server_id,
            node_id: host,
            agent_name: runtime_infos
                .iter()
                .map(|runtime| runtime.name.clone())
                .collect::<Vec<_>>()
                .join(","),
        }))
    }

    pub async fn connect_remote_over_ssh(
        &self,
        config: ServerConfig,
        ssh_credentials: SshCredentials,
        accept_unknown_host: bool,
        working_dir: Option<String>,
    ) -> Result<String, TransportError> {
        let server_id = config.server_id.clone();
        info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: connect_remote_over_ssh start server_id={} host={} ssh_port={} accept_unknown_host={} working_dir={}",
            server_id,
            ssh_credentials.host.as_str(),
            ssh_credentials.port,
            accept_unknown_host,
            working_dir.as_deref().unwrap_or("<none>")
        );
        self.app_store
            .upsert_server(&config, ServerHealthSnapshot::Connecting);
        self.app_store.update_server_connection_progress(
            server_id.as_str(),
            Some(AppConnectionProgressSnapshot::ssh_bootstrap()),
        );
        // SSH-backed sessions depend on a local tunnel that may be torn down
        // while the app is backgrounded even if the session health never
        // observed a clean disconnect. Prefer replacing any existing session
        // so resume can rebuild the full SSH transport.
        self.replace_existing_session(server_id.as_str()).await;

        // `accept_unknown_host` means "trust on first use"; a recorded
        // fingerprint that no longer matches always fails closed, including on
        // the automatic background reconnect that calls straight into here.
        //
        // We already published `Connecting` and dropped the previous session
        // above, so a refused host key has to clear that state on the way out.
        // Returning straight through `?` would strand the server in
        // `Connecting` forever and mask the typed host-key failure behind a
        // spinner that never resolves.
        let ssh_client = match crate::ssh::connect_with_host_trust(
            ssh_credentials.clone(),
            accept_unknown_host,
        )
        .await
        {
            Ok(client) => Arc::new(client),
            Err(error) => {
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "MobileClient: SSH host-key trust refused connect server_id={} host={} ssh_port={} error={}",
                    server_id,
                    ssh_credentials.host.as_str(),
                    ssh_credentials.port,
                    error
                );
                self.app_store
                    .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
                self.app_store
                    .update_server_connection_progress(server_id.as_str(), None);
                return Err(map_ssh_transport_error(error));
            }
        };
        info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: SSH transport established server_id={} host={} ssh_port={}",
            config.server_id,
            ssh_credentials.host.as_str(),
            ssh_credentials.port
        );

        let use_ipv6 = config.host.contains(':');
        let bootstrap = match ssh_client
            .bootstrap_codex_server(working_dir.as_deref(), use_ipv6)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "remote ssh bootstrap failed server={} error={}",
                    config.server_id, error
                );
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "MobileClient: remote ssh bootstrap failed server_id={} host={} error={}",
                    config.server_id,
                    ssh_credentials.host.as_str(),
                    error
                );
                ssh_client.disconnect().await;
                self.app_store
                    .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
                self.app_store
                    .update_server_connection_progress(server_id.as_str(), None);
                return Err(map_ssh_transport_error(error));
            }
        };
        info!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: remote ssh bootstrap succeeded server_id={} host={} remote_port={} local_tunnel_port={} pid={:?}",
            config.server_id,
            ssh_credentials.host.as_str(),
            bootstrap.server_port,
            bootstrap.tunnel_local_port,
            bootstrap.pid
        );

        let result = self
            .finish_connect_remote_over_ssh(
                config,
                ssh_credentials,
                accept_unknown_host,
                ssh_client,
                bootstrap,
                working_dir,
            )
            .await;
        match &result {
            Ok(_) => {
                self.app_store
                    .update_server_connection_progress(server_id.as_str(), None);
            }
            Err(_) => {
                self.app_store
                    .update_server_health(server_id.as_str(), ServerHealthSnapshot::Disconnected);
                self.app_store
                    .update_server_connection_progress(server_id.as_str(), None);
            }
        }
        result
    }

    pub(crate) async fn finish_connect_remote_over_ssh(
        &self,
        mut config: ServerConfig,
        ssh_credentials: SshCredentials,
        _accept_unknown_host: bool,
        ssh_client: Arc<SshClient>,
        bootstrap: SshBootstrapResult,
        working_dir: Option<String>,
    ) -> Result<String, TransportError> {
        let server_id = config.server_id.clone();
        trace!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: finish_connect_remote_over_ssh start server_id={} host={} bootstrap_remote_port={} bootstrap_local_port={} pid={:?}",
            server_id,
            ssh_credentials.host.as_str(),
            bootstrap.server_port,
            bootstrap.tunnel_local_port,
            bootstrap.pid
        );

        match bootstrap.transport {
            SshBootstrapTransport::AppServerProxy => {
                config.port = 0;
                config.websocket_url = Some(format!("app-server-proxy://{}", config.server_id));
            }
            SshBootstrapTransport::WebSocketTunnel => {
                config.port = bootstrap.server_port;
                config.websocket_url =
                    Some(format!("ws://127.0.0.1:{}", bootstrap.tunnel_local_port));
            }
        }
        config.is_local = false;
        config.tls = false;
        let ssh_pid = Arc::new(StdMutex::new(bootstrap.pid));
        let ssh_reconnect_transport = SshReconnectTransport::from_bootstrap(
            Arc::clone(&ssh_client),
            &bootstrap,
            working_dir,
            config.host.contains(':'),
            Arc::clone(&ssh_pid),
        );

        // Eagerly establish the Codex client now that the SSH bootstrap is up,
        // matching the multi-runtime SSH-bridges path so the multiplexed
        // session only sees populated clients.
        let (_, connect_args) = crate::session::connection::remote_connect_args(&config);
        let initial_connect = match bootstrap.transport {
            SshBootstrapTransport::AppServerProxy => {
                crate::session::connection::connect_remote_client_over_app_server_proxy(
                    &ssh_client,
                    &connect_args,
                    &bootstrap.codex_path,
                    bootstrap.shell,
                )
                .await
            }
            SshBootstrapTransport::WebSocketTunnel => {
                crate::session::connection::connect_remote_client(&connect_args).await
            }
        };
        let initial_client = match initial_connect {
            Ok(client) => client,
            Err(error) => {
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "MobileClient: remote ssh codex connect failed server_id={} host={} error={}",
                    server_id,
                    ssh_credentials.host.as_str(),
                    error
                );
                ssh_client.disconnect().await;
                return Err(error);
            }
        };
        let trait_transport: Arc<dyn crate::session::remote_transport::RemoteTransport> =
            Arc::new(ssh_reconnect_transport);
        let resource = RuntimeRemoteSessionResource {
            runtime_kind: "codex".to_string(),
            client: initial_client,
            transport: Some(trait_transport),
            keepalive: None,
        };
        let extras = RemoteSessionExtras {
            ssh_client: Some(Arc::clone(&ssh_client)),
            ssh_pid: Some(Arc::clone(&ssh_pid)),
        };
        let session = match ServerSession::connect_remote_multiplexed(
            config,
            vec![resource],
            extras,
        )
        .await
        {
            Ok(session) => Arc::new(session),
            Err(error) => {
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "remote ssh session connect failed server={} error={}",
                    server_id, error
                );
                warn!(target: super::MOBILE_CLIENT_TRACING_TARGET,
                    "MobileClient: remote ssh session connect failed server_id={} host={} error={}",
                    server_id,
                    ssh_credentials.host.as_str(),
                    error
                );
                ssh_client.disconnect().await;
                return Err(error);
            }
        };

        trace!(target: super::MOBILE_CLIENT_TRACING_TARGET,
            "MobileClient: finish_connect_remote_over_ssh session connected server_id={} websocket_url={}",
            server_id,
            session
                .config()
                .websocket_url
                .as_deref()
                .unwrap_or("<none>")
        );
        let codex_runtime_info = AgentRuntimeInfo {
            kind: "codex".to_string(),
            name: "codex".to_string(),
            display_name: "Codex".to_string(),
            available: true,
        };
        self.attach_remote_session(&server_id, session, vec![codex_runtime_info]);

        info!(target: super::MOBILE_CLIENT_TRACING_TARGET, "MobileClient: connected remote SSH server {server_id}");
        Ok(server_id)
    }
}
