//! Shared reconnection logic for iOS and Android.
//!
//! Consolidates the duplicated transport-resolution and reconnect-plan
//! computation that previously lived in platform Swift/Kotlin code.

use crate::mobile_client::MobileClient;
use crate::session::connection::{ConnectionHealth, InProcessConfig, ServerConfig};
use crate::slingshot_url::is_slingshot_connection_url;
use crate::slingshot_url::parse_slingshot_connection_url;
use crate::ssh::{SshAuth, SshClient, SshCredentials};
use crate::types::AgentRuntimeKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

const HOT_RECONNECT_SETTLE_DEADLINE: Duration = Duration::from_secs(5);

// ── UniFFI boundary types ───────────────────────────────────────────────

/// Mirrors the platform `SavedServer` data class / struct.
#[derive(Clone, Debug, uniffi::Record)]
pub struct SavedServerRecord {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub port: u16,
    pub codex_ports: Vec<u16>,
    pub ssh_port: Option<u16>,
    pub source: String,
    pub has_codex_server: bool,
    pub wake_mac: Option<String>,
    pub preferred_connection_mode: Option<String>,
    pub preferred_codex_port: Option<u16>,
    pub ssh_port_forwarding_enabled: Option<bool>,
    pub websocket_url: Option<String>,
    pub remembered_by_user: bool,
    /// `None` identifies a non-bridge saved server. `Some([])` identifies an
    /// SSH bridge whose available runtimes should be probed on reconnect;
    /// `Some(kinds)` reconnects only the selected runtime kinds.
    pub ssh_bridge_runtime_kinds: Option<Vec<AgentRuntimeKind>>,
}

/// SSH auth method discriminator.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum SshAuthMethodRecord {
    Password,
    Key,
}

/// SSH credential record passed across the FFI boundary.
#[derive(Clone, Debug, uniffi::Record)]
pub struct SshCredentialRecord {
    pub username: String,
    pub auth_method: SshAuthMethodRecord,
    pub password: Option<String>,
    pub private_key_pem: Option<String>,
    pub passphrase: Option<String>,
    pub unlock_macos_keychain: bool,
}

/// ChatGPT credentials supplied by the platform keychain/token store.
#[derive(Clone, Debug, uniffi::Record)]
pub struct SlingshotCredentialRecord {
    pub access_token: String,
    pub account_id: String,
}

/// Result of a single server reconnection attempt.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ReconnectResult {
    pub server_id: String,
    pub success: bool,
    pub needs_local_auth_restore: bool,
    pub outcome: ReconnectOutcome,
    pub error_message: Option<String>,
}

/// Typed result for reconnect orchestration.
///
/// `success` remains on [`ReconnectResult`] for source compatibility with
/// existing platform callers. New callers should use this enum to distinguish
/// a completed connection from an already-healthy/self-healing session and
/// from actionable credential, pairing, or configuration failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ReconnectOutcome {
    Connected,
    AlreadyConnected,
    SelfHealing,
    Coalesced,
    Cancelled,
    MissingSshCredential,
    MissingSlingshotCredential,
    RePairRequired,
    RepairRequired,
    ConfigurationRequired,
    NotFound,
    Failed,
}

impl ReconnectResult {
    pub(crate) fn completed(
        server_id: impl Into<String>,
        outcome: ReconnectOutcome,
        needs_local_auth_restore: bool,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            success: true,
            needs_local_auth_restore,
            outcome,
            error_message: None,
        }
    }

    pub(crate) fn blocked(
        server_id: impl Into<String>,
        outcome: ReconnectOutcome,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            success: false,
            needs_local_auth_restore: false,
            outcome,
            error_message: Some(error_message.into()),
        }
    }

    pub(crate) fn failed(server_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self::blocked(server_id, ReconnectOutcome::Failed, error_message)
    }
}

/// Callback interface for platform-side SSH credential storage.
#[uniffi::export(callback_interface)]
pub trait SshCredentialProvider: Send + Sync {
    fn load_credential(&self, host: String, port: u16) -> Option<SshCredentialRecord>;
}

/// Callback interface for platform-side ChatGPT credential storage.
#[uniffi::export(callback_interface)]
pub trait SlingshotCredentialProvider: Send + Sync {
    fn load_credential(&self) -> Option<SlingshotCredentialRecord>;
}

// ── Internal reconnect plan ─────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) enum ReconnectPlan {
    Ssh {
        server_id: String,
        display_name: String,
        host: String,
        ssh_port: u16,
        credential: SshCredentialRecord,
    },
    SshBridge {
        server_id: String,
        display_name: String,
        host: String,
        ssh_port: u16,
        credential: SshCredentialRecord,
        runtime_kinds: Vec<AgentRuntimeKind>,
    },
    Local {
        server_id: String,
        display_name: String,
    },
    DirectRemote {
        server_id: String,
        display_name: String,
        host: String,
        port: u16,
    },
    RemoteUrl {
        server_id: String,
        display_name: String,
        websocket_url: String,
    },
    Slingshot {
        server_id: String,
        display_name: String,
        base_url: String,
        environment_id: String,
        credential: SlingshotCredentialRecord,
    },
}

impl ReconnectPlan {
    pub(crate) fn server_id(&self) -> &str {
        match self {
            Self::Ssh { server_id, .. }
            | Self::SshBridge { server_id, .. }
            | Self::Local { server_id, .. }
            | Self::DirectRemote { server_id, .. }
            | Self::RemoteUrl { server_id, .. }
            | Self::Slingshot { server_id, .. } => server_id,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "the decision carries an owned reconnect plan directly to avoid heap allocation in the synchronous planning path"
)]
pub(crate) enum ReconnectPlanDecision {
    Plan(ReconnectPlan),
    NoAction(ReconnectOutcome),
}

// ── Transport resolution helpers ────────────────────────────────────────

/// Resolve the effective preferred connection mode, handling legacy
/// `ssh_port_forwarding_enabled` migration.
///
/// Mirrors iOS `migratedPreferredConnectionMode` and Android
/// `resolvedPreferredConnectionMode` (simplified — the full Android version
/// also validates that the mode is still reachable, but for reconnect
/// planning the raw preference is what matters since we skip if no
/// credential is available anyway).
pub(crate) fn resolved_preferred_connection_mode(server: &SavedServerRecord) -> Option<String> {
    if let Some(ref mode) = server.preferred_connection_mode {
        return Some(mode.clone());
    }
    if server.ssh_port_forwarding_enabled == Some(true) {
        return Some("ssh".to_string());
    }
    None
}

/// Resolve the SSH port for a saved server.
///
/// Mirrors Android `resolvedSshPort`:
///   `sshPort ?: port.takeIf { !hasCodexServer && it > 0 } ?: 22`
/// and iOS `SavedServer.toDiscoveredServer()` → `DiscoveredServer.resolvedSSHPort`:
///   `sshPort ?? (hasCodexServer ? nil : port)` then `?? 22`
pub(crate) fn resolved_ssh_port(server: &SavedServerRecord) -> u16 {
    if let Some(port) = server.ssh_port {
        return port;
    }
    if !server.has_codex_server && server.port > 0 {
        return server.port;
    }
    22
}

/// Build the list of available direct Codex ports (merging port + codex_ports).
///
/// Mirrors Android `availableDirectCodexPorts`.
fn available_direct_codex_ports(server: &SavedServerRecord) -> Vec<u16> {
    let mut ordered = Vec::new();
    if server.has_codex_server && server.port > 0 {
        ordered.push(server.port);
    }
    for &p in &server.codex_ports {
        if p > 0 && !ordered.contains(&p) {
            ordered.push(p);
        }
    }
    ordered
}

/// Whether the server prefers SSH connections.
fn prefers_ssh(server: &SavedServerRecord) -> bool {
    resolved_preferred_connection_mode(server).as_deref() == Some("ssh")
}

/// Whether the server requires user to choose a connection mode before
/// we can auto-connect.
fn requires_connection_choice(server: &SavedServerRecord) -> bool {
    if server.websocket_url.is_some() {
        return false;
    }
    let mode = resolved_preferred_connection_mode(server);
    if mode.is_some() {
        return false;
    }
    let ports = available_direct_codex_ports(server);
    let can_ssh = can_connect_via_ssh(server);
    ports.len() > 1 || (!ports.is_empty() && can_ssh)
}

/// Whether SSH is a viable transport for this server.
fn can_connect_via_ssh(server: &SavedServerRecord) -> bool {
    if server.websocket_url.is_some() {
        return false;
    }
    server.ssh_port.is_some()
        || server.source == "ssh"
        || (!server.has_codex_server && resolved_ssh_port(server) > 0)
        || server.preferred_connection_mode.as_deref() == Some("ssh")
        || server.ssh_port_forwarding_enabled == Some(true)
}

/// Resolve the preferred codex port for direct-codex mode.
fn resolved_preferred_codex_port(server: &SavedServerRecord) -> Option<u16> {
    if resolved_preferred_connection_mode(server).as_deref() != Some("directCodex") {
        return None;
    }
    let ports = available_direct_codex_ports(server);
    if let Some(pref) = server.preferred_codex_port
        && ports.contains(&pref)
    {
        return Some(pref);
    }
    None
}

/// Resolve the direct Codex port for a saved server.
///
/// Returns `None` when SSH is preferred, when the user needs to choose,
/// or when no direct port is available.
///
/// Mirrors Android `directCodexPort`.
pub(crate) fn direct_codex_port(server: &SavedServerRecord) -> Option<u16> {
    if server.websocket_url.is_some() {
        return None;
    }
    if prefers_ssh(server) {
        return None;
    }
    if let Some(port) = resolved_preferred_codex_port(server) {
        return Some(port);
    }
    if requires_connection_choice(server) {
        return None;
    }
    let ports = available_direct_codex_ports(server);
    ports.first().copied()
}

// ── Plan computation ────────────────────────────────────────────────────

/// Compute the reconnect plan for a single saved server.
///
/// Consolidates iOS `reconnectPlan(for:)` and Android
/// `reconnectSavedServer` into a single decision tree.
#[cfg(test)]
fn compute_reconnect_plan(
    server: &SavedServerRecord,
    credential: Option<&SshCredentialRecord>,
    is_connected: bool,
) -> Option<ReconnectPlan> {
    compute_reconnect_plan_with_slingshot(server, credential, None, is_connected)
}

#[cfg(test)]
pub(crate) fn compute_reconnect_plan_with_slingshot(
    server: &SavedServerRecord,
    credential: Option<&SshCredentialRecord>,
    slingshot_credential: Option<&SlingshotCredentialRecord>,
    is_connected: bool,
) -> Option<ReconnectPlan> {
    match decide_reconnect_plan_with_slingshot(
        server,
        credential,
        slingshot_credential,
        is_connected,
    ) {
        ReconnectPlanDecision::Plan(plan) => Some(plan),
        ReconnectPlanDecision::NoAction(_) => None,
    }
}

pub(crate) fn decide_reconnect_plan_with_slingshot(
    server: &SavedServerRecord,
    credential: Option<&SshCredentialRecord>,
    slingshot_credential: Option<&SlingshotCredentialRecord>,
    is_connected: bool,
) -> ReconnectPlanDecision {
    // 1. Skip if already connected
    if is_connected {
        return ReconnectPlanDecision::NoAction(ReconnectOutcome::AlreadyConnected);
    }

    // 2. SSH bridge records reconnect as one multiplexed in-process bridge
    // group. Classification is explicit so mixed direct/SSH records keep
    // their normal transport selection behavior.
    if let Some(runtime_kinds) = server.ssh_bridge_runtime_kinds.as_ref() {
        if let Some(cred) = credential {
            return ReconnectPlanDecision::Plan(ReconnectPlan::SshBridge {
                server_id: server.id.clone(),
                display_name: server.name.clone(),
                host: server.hostname.clone(),
                ssh_port: resolved_ssh_port(server),
                credential: cred.clone(),
                runtime_kinds: normalized_ssh_bridge_runtime_kinds(runtime_kinds),
            });
        }
        return ReconnectPlanDecision::NoAction(ReconnectOutcome::MissingSshCredential);
    }

    // 3. WebSocket URL override -> RemoteUrl
    if let Some(ref ws_url) = server.websocket_url {
        // Slingshot URLs are saved-server markers. They require the
        // ChatGPT-token-aware Slingshot connector, not the generic websocket
        // transport.
        if is_slingshot_connection_url(ws_url) {
            let Some(slingshot) = parse_slingshot_connection_url(ws_url) else {
                return ReconnectPlanDecision::NoAction(ReconnectOutcome::ConfigurationRequired);
            };
            let Some(credential) = slingshot_credential else {
                return ReconnectPlanDecision::NoAction(
                    ReconnectOutcome::MissingSlingshotCredential,
                );
            };
            return ReconnectPlanDecision::Plan(ReconnectPlan::Slingshot {
                server_id: server.id.clone(),
                display_name: server.name.clone(),
                base_url: slingshot.base_url,
                environment_id: slingshot.environment_id,
                credential: credential.clone(),
            });
        }
        return ReconnectPlanDecision::Plan(ReconnectPlan::RemoteUrl {
            server_id: server.id.clone(),
            display_name: server.name.clone(),
            websocket_url: ws_url.clone(),
        });
    }

    let mode = resolved_preferred_connection_mode(server);

    // 4. Explicit SSH mode + credential → Ssh
    if mode.as_deref() == Some("ssh") {
        if let Some(cred) = credential {
            return ReconnectPlanDecision::Plan(ReconnectPlan::Ssh {
                server_id: server.id.clone(),
                display_name: server.name.clone(),
                host: server.hostname.clone(),
                ssh_port: resolved_ssh_port(server),
                credential: cred.clone(),
            });
        }
        // SSH preferred but no credential — cannot reconnect
        return ReconnectPlanDecision::NoAction(ReconnectOutcome::MissingSshCredential);
    }

    // 5. Direct Codex port available → DirectRemote
    if let Some(port) = direct_codex_port(server) {
        return ReconnectPlanDecision::Plan(ReconnectPlan::DirectRemote {
            server_id: server.id.clone(),
            display_name: server.name.clone(),
            host: server.hostname.clone(),
            port,
        });
    }

    // 6. No explicit mode, but credential available → SSH (legacy fallback)
    if mode.is_none()
        && let Some(cred) = credential
    {
        return ReconnectPlanDecision::Plan(ReconnectPlan::Ssh {
            server_id: server.id.clone(),
            display_name: server.name.clone(),
            host: server.hostname.clone(),
            ssh_port: resolved_ssh_port(server),
            credential: cred.clone(),
        });
    }

    // 7. Local source → Local
    if server.source == "local" {
        return ReconnectPlanDecision::Plan(ReconnectPlan::Local {
            server_id: server.id.clone(),
            display_name: server.name.clone(),
        });
    }

    // 8. No viable transport.
    ReconnectPlanDecision::NoAction(ReconnectOutcome::ConfigurationRequired)
}

// ── Plan execution ──────────────────────────────────────────────────────

pub(crate) fn reconnect_outcome_for_health(
    health: &ConnectionHealth,
    has_degraded_runtime: bool,
    has_connecting_runtime: bool,
) -> Option<ReconnectOutcome> {
    if has_degraded_runtime {
        return None;
    }
    if has_connecting_runtime {
        return Some(ReconnectOutcome::SelfHealing);
    }
    match health {
        ConnectionHealth::Connected => Some(ReconnectOutcome::AlreadyConnected),
        ConnectionHealth::Connecting { .. } => Some(ReconnectOutcome::SelfHealing),
        ConnectionHealth::Disconnected | ConnectionHealth::Unresponsive { .. } => None,
    }
}

/// Execute a single reconnect plan against the shared `MobileClient`.
pub(crate) async fn execute_reconnect_plan(
    plan: &ReconnectPlan,
    client: &MobileClient,
) -> ReconnectResult {
    if client
        .connection_reconnect_state(plan.server_id())
        .is_some_and(|state| state.has_degraded_runtime && state.has_connecting_runtime)
    {
        client
            .wait_for_runtime_reconnects_to_settle(plan.server_id(), HOT_RECONNECT_SETTLE_DEADLINE)
            .await;
    }

    let reconnect_state = client.connection_reconnect_state(plan.server_id());
    if let Some(state) = reconnect_state.as_ref() {
        if state.has_degraded_runtime && state.has_connecting_runtime {
            return ReconnectResult::completed(
                plan.server_id(),
                ReconnectOutcome::SelfHealing,
                false,
            );
        }
        if let Some(outcome) = reconnect_outcome_for_health(
            &state.aggregate,
            state.has_degraded_runtime,
            state.has_connecting_runtime,
        ) {
            return ReconnectResult::completed(plan.server_id(), outcome, false);
        }
    }
    let cold_repair_required = reconnect_state
        .as_ref()
        .is_some_and(|state| state.has_degraded_runtime);
    let cold_guard = if cold_repair_required {
        match client.cold_reconnect_guard(plan.server_id()) {
            Some(guard) => Some(guard),
            None => {
                return ReconnectResult::completed(
                    plan.server_id(),
                    ReconnectOutcome::SelfHealing,
                    false,
                );
            }
        }
    } else {
        None
    };

    match plan {
        ReconnectPlan::Ssh {
            server_id,
            display_name,
            host,
            ssh_port,
            credential,
        } => {
            info!(
                "reconnect: executing SSH plan server_id={} host={} ssh_port={}",
                server_id, host, ssh_port
            );
            let auth = match credential.auth_method {
                SshAuthMethodRecord::Password => {
                    SshAuth::Password(credential.password.clone().unwrap_or_default())
                }
                SshAuthMethodRecord::Key => SshAuth::PrivateKey {
                    key_pem: credential.private_key_pem.clone().unwrap_or_default(),
                    passphrase: credential.passphrase.clone(),
                },
            };
            let ssh_creds = SshCredentials {
                host: host.clone(),
                port: *ssh_port,
                username: credential.username.clone(),
                auth,
                unlock_macos_keychain: credential.unlock_macos_keychain,
            };
            let config = ServerConfig {
                server_id: server_id.clone(),
                display_name: display_name.clone(),
                host: host.clone(),
                port: 0,
                websocket_url: None,
                is_local: false,
                tls: false,
            };
            match client
                .connect_remote_over_ssh(config, ssh_creds, true, None)
                .await
            {
                Ok(_) => ReconnectResult::completed(server_id, ReconnectOutcome::Connected, false),
                Err(e) => {
                    warn!(
                        "reconnect: SSH plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
        ReconnectPlan::SshBridge {
            server_id,
            display_name,
            host,
            ssh_port,
            credential,
            runtime_kinds,
        } => {
            info!(
                "reconnect: executing SSH bridge plan server_id={} host={} ssh_port={} runtimes={:?}",
                server_id, host, ssh_port, runtime_kinds
            );
            let auth = match credential.auth_method {
                SshAuthMethodRecord::Password => {
                    SshAuth::Password(credential.password.clone().unwrap_or_default())
                }
                SshAuthMethodRecord::Key => SshAuth::PrivateKey {
                    key_pem: credential.private_key_pem.clone().unwrap_or_default(),
                    passphrase: credential.passphrase.clone(),
                },
            };
            let ssh_creds = SshCredentials {
                host: host.clone(),
                port: *ssh_port,
                username: credential.username.clone(),
                auth,
                unlock_macos_keychain: credential.unlock_macos_keychain,
            };
            // Trust-on-first-use only: a saved server whose fingerprint was
            // never recorded gets pinned on the first successful reconnect. A
            // host that presents a *different* key than the recorded pin is
            // refused here, before any credential is offered.
            let ssh_client = match crate::ssh::connect_with_host_trust(ssh_creds, true).await {
                Ok(client) => Arc::new(client),
                Err(e) => {
                    warn!(
                        "reconnect: SSH bridge plan failed to connect server_id={} error={}",
                        server_id, e
                    );
                    return ReconnectResult::failed(server_id, e.to_string());
                }
            };
            let selected = match resolve_ssh_bridge_runtime_kinds(
                Arc::clone(&ssh_client),
                runtime_kinds,
            )
            .await
            {
                Ok(selected) => selected,
                Err(error) => return ReconnectResult::failed(server_id, error),
            };
            let state_root = match ssh_bridge_state_root(host) {
                Ok(path) => path,
                Err(error) => {
                    return ReconnectResult::failed(server_id, error);
                }
            };
            let reconnect = match cold_guard.clone() {
                Some(guard) => {
                    client
                        .reconnect_remote_over_ssh_bridges(
                            ssh_client,
                            server_id.clone(),
                            display_name.clone(),
                            host.clone(),
                            state_root,
                            selected,
                            crate::ssh_bridge::SshBridgeTransport::Ephemeral,
                            guard,
                        )
                        .await
                }
                None => client
                    .connect_remote_over_ssh_bridges(
                        ssh_client,
                        server_id.clone(),
                        display_name.clone(),
                        host.clone(),
                        state_root,
                        selected,
                        crate::ssh_bridge::SshBridgeTransport::Ephemeral,
                    )
                    .await
                    .map(Some),
            };
            match reconnect {
                Ok(Some(_)) => {
                    ReconnectResult::completed(server_id, ReconnectOutcome::Connected, false)
                }
                Ok(None) => {
                    ReconnectResult::completed(server_id, ReconnectOutcome::SelfHealing, false)
                }
                Err(e) => {
                    warn!(
                        "reconnect: SSH bridge plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
        ReconnectPlan::Local {
            server_id,
            display_name,
        } => {
            info!("reconnect: executing Local plan server_id={}", server_id);
            let config = ServerConfig {
                server_id: server_id.clone(),
                display_name: display_name.clone(),
                host: "127.0.0.1".to_string(),
                port: 0,
                websocket_url: None,
                is_local: true,
                tls: false,
            };
            match client
                .connect_local(config, InProcessConfig::default())
                .await
            {
                Ok(_) => ReconnectResult::completed(server_id, ReconnectOutcome::Connected, true),
                Err(e) => {
                    warn!(
                        "reconnect: Local plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
        ReconnectPlan::DirectRemote {
            server_id,
            display_name,
            host,
            port,
        } => {
            info!(
                "reconnect: executing DirectRemote plan server_id={} host={} port={}",
                server_id, host, port
            );
            let config = ServerConfig {
                server_id: server_id.clone(),
                display_name: display_name.clone(),
                host: host.clone(),
                port: *port,
                websocket_url: None,
                is_local: false,
                tls: false,
            };
            match client.connect_remote(config).await {
                Ok(_) => ReconnectResult::completed(server_id, ReconnectOutcome::Connected, false),
                Err(e) => {
                    warn!(
                        "reconnect: DirectRemote plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
        ReconnectPlan::RemoteUrl {
            server_id,
            display_name,
            websocket_url,
        } => {
            info!(
                "reconnect: executing RemoteUrl plan server_id={} url={}",
                server_id, websocket_url
            );
            let config = ServerConfig {
                server_id: server_id.clone(),
                display_name: display_name.clone(),
                host: String::new(),
                port: 0,
                websocket_url: Some(websocket_url.clone()),
                is_local: false,
                tls: false,
            };
            match client.connect_remote(config).await {
                Ok(_) => ReconnectResult::completed(server_id, ReconnectOutcome::Connected, false),
                Err(e) => {
                    warn!(
                        "reconnect: RemoteUrl plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
        ReconnectPlan::Slingshot {
            server_id,
            display_name,
            base_url,
            environment_id,
            credential,
        } => {
            info!(
                "reconnect: executing Slingshot plan server_id={} environment_id={}",
                server_id, environment_id
            );
            match client
                .connect_remote_over_slingshot(
                    server_id.clone(),
                    display_name.clone(),
                    base_url.clone(),
                    credential.access_token.clone(),
                    credential.account_id.clone(),
                    environment_id.clone(),
                    String::new(),
                )
                .await
            {
                Ok(_) => ReconnectResult::completed(server_id, ReconnectOutcome::Connected, false),
                Err(e) => {
                    warn!(
                        "reconnect: Slingshot plan failed server_id={} error={}",
                        server_id, e
                    );
                    ReconnectResult::failed(server_id, e.to_string())
                }
            }
        }
    }
}

fn normalized_ssh_bridge_runtime_kinds(values: &[AgentRuntimeKind]) -> Vec<AgentRuntimeKind> {
    values
        .iter()
        .filter_map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "" => None,
            "codex" => Some("codex".to_string()),
            "claude" => Some("claude".to_string()),
            "pi" => Some("pi".to_string()),
            "opencode" | "open-code" | "open_code" => Some("opencode".to_string()),
            other => Some(other.to_string()),
        })
        .fold(Vec::new(), |mut acc, kind| {
            if !acc.contains(&kind) {
                acc.push(kind);
            }
            acc
        })
}

async fn resolve_ssh_bridge_runtime_kinds(
    ssh_client: Arc<SshClient>,
    requested: &[AgentRuntimeKind],
) -> Result<Vec<AgentRuntimeKind>, String> {
    let availability = crate::ssh_bridge::probe_remote_agents(&ssh_client)
        .await
        .unwrap_or_default();
    info!(
        "reconnect: SSH bridge agent availability requested={:?} availability={:?}",
        requested, availability
    );
    select_ssh_bridge_runtime_kinds(requested, &availability)
}

fn select_ssh_bridge_runtime_kinds(
    requested: &[AgentRuntimeKind],
    availability: &[crate::ssh_bridge::RemoteAgentAvailability],
) -> Result<Vec<AgentRuntimeKind>, String> {
    let available = |kind: &AgentRuntimeKind| match kind.as_str() {
        "codex" => true,
        "claude" | "pi" | "opencode" => availability.iter().any(|entry| {
            &entry.kind == kind
                && entry.status == crate::ssh_bridge::AgentAvailabilityStatus::Available
        }),
        _ => false,
    };

    let candidates = if requested.is_empty() {
        vec![
            "claude".to_string(),
            "pi".to_string(),
            "opencode".to_string(),
            "codex".to_string(),
        ]
    } else {
        requested.to_vec()
    };
    let mut selected =
        candidates
            .into_iter()
            .filter(|kind| available(kind))
            .fold(Vec::new(), |mut acc, kind| {
                if !acc.contains(&kind) {
                    acc.push(kind);
                }
                acc
            });
    if selected.is_empty() && requested.is_empty() {
        selected.push("codex".to_string());
    }
    if selected.is_empty() {
        return Err(format!(
            "none of the explicitly selected SSH bridge runtimes are available: {}",
            requested.join(", ")
        ));
    }
    info!(
        "reconnect: SSH bridge selected runtimes requested={:?} selected={:?}",
        requested, selected
    );
    Ok(selected)
}

fn ssh_bridge_state_root(host: &str) -> Result<String, String> {
    let path = ssh_bridge_state_path(
        std::env::var_os("HOME").map(PathBuf::from),
        host,
        cfg!(target_os = "android"),
    );
    std::fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create SSH bridge state dir {:?}: {error}", path))?;
    Ok(path.to_string_lossy().into_owned())
}

fn ssh_bridge_state_path(home: Option<PathBuf>, host: &str, is_android: bool) -> PathBuf {
    let home = home.unwrap_or_else(std::env::temp_dir);
    let base = if is_android {
        home
    } else {
        home.join("Library").join("Application Support")
    };
    base.join("remora-bridges")
        .join(percent_encode_alphanumeric(host))
}

fn percent_encode_alphanumeric(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

// ── Unit tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_health_short_circuit_requires_every_selected_runtime_to_be_repairable() {
        assert_eq!(
            reconnect_outcome_for_health(&ConnectionHealth::Connected, false, false),
            Some(ReconnectOutcome::AlreadyConnected)
        );
        assert_eq!(
            reconnect_outcome_for_health(
                &ConnectionHealth::Connecting {
                    attempt: 2,
                    max_attempts: 5,
                },
                false,
                true,
            ),
            Some(ReconnectOutcome::SelfHealing)
        );
        assert_eq!(
            reconnect_outcome_for_health(&ConnectionHealth::Connected, true, false),
            None,
            "aggregate Connected must not hide an exhausted selected runtime"
        );
        assert_eq!(
            reconnect_outcome_for_health(&ConnectionHealth::Connected, false, true),
            Some(ReconnectOutcome::SelfHealing),
            "aggregate Connected must not hide a selected runtime's active hot recovery"
        );
    }

    fn base_server() -> SavedServerRecord {
        SavedServerRecord {
            id: "srv-1".into(),
            name: "Test Server".into(),
            hostname: "192.168.1.100".into(),
            port: 8080,
            codex_ports: vec![],
            ssh_port: None,
            source: "manual".into(),
            has_codex_server: true,
            wake_mac: None,
            preferred_connection_mode: None,
            preferred_codex_port: None,
            ssh_port_forwarding_enabled: None,
            websocket_url: None,
            remembered_by_user: true,
            ssh_bridge_runtime_kinds: None,
        }
    }

    #[test]
    fn ssh_bridge_state_path_matches_platform_storage_roots() {
        let android_home = PathBuf::from("/data/user/0/com.remora.android/files");
        assert_eq!(
            ssh_bridge_state_path(Some(android_home.clone()), "host.local:22", true),
            android_home
                .join("remora-bridges")
                .join("host%2Elocal%3A22")
        );

        let apple_home = PathBuf::from("/Users/remora");
        assert_eq!(
            ssh_bridge_state_path(Some(apple_home.clone()), "host.local:22", false),
            apple_home
                .join("Library")
                .join("Application Support")
                .join("remora-bridges")
                .join("host%2Elocal%3A22")
        );
    }

    #[test]
    fn ssh_bridge_state_host_encoding_uses_utf8_bytes() {
        assert_eq!(
            percent_encode_alphanumeric("host_name-1"),
            "host%5Fname%2D1"
        );
        assert_eq!(percent_encode_alphanumeric("café"), "caf%C3%A9");
    }

    fn ssh_credential() -> SshCredentialRecord {
        SshCredentialRecord {
            username: "user".into(),
            auth_method: SshAuthMethodRecord::Password,
            password: Some("pass".into()),
            private_key_pem: None,
            passphrase: None,
            unlock_macos_keychain: false,
        }
    }

    // -- host-key trust on the automatic reconnect paths --
    //
    // Automatic reconnect runs without any user present, so a host that
    // presents a different key than the one we recorded must be refused
    // *before* the stored SSH password is offered. Both reconnect arms are
    // driven end-to-end against a real in-process SSH server that answers
    // with host key B while the trust store pins host key A.

    /// Register a pin for the test server's address, run `body`, then drop
    /// the process-wide store again.
    async fn with_pinned_mismatch<F, Fut, T>(
        server: &crate::ssh::test_server::TestSshServer,
        body: F,
    ) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let _guard = crate::ssh::HOST_TRUST_TEST_LOCK.lock().await;
        let store = crate::ssh::test_server::in_memory_trust_store();
        let pinned_key = crate::ssh::test_server::test_host_key();
        let pinned = crate::ssh::test_server::host_key_fingerprint(&pinned_key);
        store.pin(server.host.clone(), server.port, pinned).unwrap();
        crate::ssh::register_host_trust_store(store);
        let result = body().await;
        crate::ssh::clear_host_trust_store();
        result
    }

    #[tokio::test]
    async fn ssh_reconnect_plan_refuses_a_changed_host_key() {
        let server =
            crate::ssh::test_server::TestSshServer::start(crate::ssh::test_server::test_host_key())
                .await;
        let plan = ReconnectPlan::Ssh {
            server_id: "srv-host-key-ssh".into(),
            display_name: "Test".into(),
            host: server.host.clone(),
            ssh_port: server.port,
            credential: ssh_credential(),
        };
        let result = with_pinned_mismatch(&server, || async {
            let client = MobileClient::new();
            execute_reconnect_plan(&plan, &client).await
        })
        .await;

        assert!(
            !result.success,
            "reconnect must not succeed on a changed host key"
        );
        let message = result.error_message.unwrap_or_default();
        assert!(
            message.contains("host-key-changed:"),
            "expected a typed host-key-changed failure, got {message:?}"
        );
        assert_eq!(
            server.auth_attempts(),
            0,
            "the saved SSH credential must never be offered to a host whose key changed"
        );
    }

    #[tokio::test]
    async fn ssh_bridge_reconnect_plan_refuses_a_changed_host_key() {
        let server =
            crate::ssh::test_server::TestSshServer::start(crate::ssh::test_server::test_host_key())
                .await;
        let plan = ReconnectPlan::SshBridge {
            server_id: "srv-host-key-bridge".into(),
            display_name: "Test".into(),
            host: server.host.clone(),
            ssh_port: server.port,
            credential: ssh_credential(),
            runtime_kinds: vec!["codex".to_string()],
        };
        let result = with_pinned_mismatch(&server, || async {
            let client = MobileClient::new();
            execute_reconnect_plan(&plan, &client).await
        })
        .await;

        assert!(
            !result.success,
            "ssh-bridge reconnect must not succeed on a changed host key"
        );
        let message = result.error_message.unwrap_or_default();
        assert!(
            message.contains("host-key-changed:"),
            "expected a typed host-key-changed failure, got {message:?}"
        );
        assert_eq!(
            server.auth_attempts(),
            0,
            "the saved SSH credential must never be offered to a host whose key changed"
        );
    }

    // -- resolved_preferred_connection_mode tests --

    #[test]
    fn resolved_mode_explicit_ssh() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("ssh".into());
        assert_eq!(
            resolved_preferred_connection_mode(&s).as_deref(),
            Some("ssh")
        );
    }

    #[test]
    fn resolved_mode_explicit_direct_codex() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("directCodex".into());
        assert_eq!(
            resolved_preferred_connection_mode(&s).as_deref(),
            Some("directCodex")
        );
    }

    #[test]
    fn resolved_mode_legacy_ssh_port_forwarding_enabled() {
        let mut s = base_server();
        s.ssh_port_forwarding_enabled = Some(true);
        assert_eq!(
            resolved_preferred_connection_mode(&s).as_deref(),
            Some("ssh")
        );
    }

    #[test]
    fn resolved_mode_legacy_ssh_port_forwarding_disabled() {
        let mut s = base_server();
        s.ssh_port_forwarding_enabled = Some(false);
        assert!(resolved_preferred_connection_mode(&s).is_none());
    }

    #[test]
    fn resolved_mode_none_when_no_preference() {
        let s = base_server();
        assert!(resolved_preferred_connection_mode(&s).is_none());
    }

    // -- resolved_ssh_port tests --

    #[test]
    fn resolved_ssh_port_explicit() {
        let mut s = base_server();
        s.ssh_port = Some(2222);
        assert_eq!(resolved_ssh_port(&s), 2222);
    }

    #[test]
    fn resolved_ssh_port_fallback_to_port_when_no_codex() {
        let mut s = base_server();
        s.has_codex_server = false;
        s.port = 3333;
        assert_eq!(resolved_ssh_port(&s), 3333);
    }

    #[test]
    fn resolved_ssh_port_default_22_when_has_codex() {
        let s = base_server();
        assert_eq!(resolved_ssh_port(&s), 22);
    }

    #[test]
    fn resolved_ssh_port_default_22_when_port_zero() {
        let mut s = base_server();
        s.has_codex_server = false;
        s.port = 0;
        assert_eq!(resolved_ssh_port(&s), 22);
    }

    // -- direct_codex_port tests --

    #[test]
    fn direct_codex_port_returns_port_when_has_codex() {
        let s = base_server();
        // has_codex_server=true, port=8080, no SSH preference → port 8080
        assert_eq!(direct_codex_port(&s), Some(8080));
    }

    #[test]
    fn direct_codex_port_none_when_ssh_preferred() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("ssh".into());
        assert_eq!(direct_codex_port(&s), None);
    }

    #[test]
    fn direct_codex_port_preferred_codex_port_in_direct_mode() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("directCodex".into());
        s.codex_ports = vec![9090, 9091];
        s.preferred_codex_port = Some(9091);
        assert_eq!(direct_codex_port(&s), Some(9091));
    }

    #[test]
    fn direct_codex_port_none_when_requires_choice() {
        let mut s = base_server();
        // Two ports + SSH available + no preferred mode → requires choice
        s.codex_ports = vec![9090, 9091];
        s.ssh_port = Some(22);
        assert_eq!(direct_codex_port(&s), None);
    }

    #[test]
    fn direct_codex_port_none_when_no_codex() {
        let mut s = base_server();
        s.has_codex_server = false;
        s.port = 22;
        s.codex_ports = vec![];
        assert_eq!(direct_codex_port(&s), None);
    }

    #[test]
    fn direct_codex_port_none_when_websocket_url() {
        let mut s = base_server();
        s.websocket_url = Some("wss://example.com/ws".into());
        assert_eq!(direct_codex_port(&s), None);
    }

    // -- compute_reconnect_plan tests --

    #[test]
    fn plan_skip_when_connected() {
        let s = base_server();
        assert!(compute_reconnect_plan(&s, None, true).is_none());
    }

    #[test]
    fn plan_remote_url_when_websocket_set() {
        let mut s = base_server();
        s.websocket_url = Some("wss://example.com/ws".into());
        let plan = compute_reconnect_plan(&s, None, false);
        assert!(matches!(plan, Some(ReconnectPlan::RemoteUrl { .. })));
    }

    #[test]
    fn plan_skips_slingshot_marker_url() {
        let mut s = base_server();
        s.websocket_url =
            Some("slingshot://env_123?baseUrl=https://chatgpt.com/backend-api".into());
        let plan = compute_reconnect_plan(&s, None, false);
        assert!(plan.is_none());
    }

    #[test]
    fn plan_slingshot_when_marker_url_and_credential_available() {
        let mut s = base_server();
        s.websocket_url = Some("slingshot://env_123?baseUrl=https://chatgpt.com".into());
        let credential = SlingshotCredentialRecord {
            access_token: "access-token".into(),
            account_id: "acct".into(),
        };

        let plan = compute_reconnect_plan_with_slingshot(&s, None, Some(&credential), false);

        match plan {
            Some(ReconnectPlan::Slingshot {
                server_id,
                display_name,
                base_url,
                environment_id,
                credential,
            }) => {
                assert_eq!(server_id, "srv-1");
                assert_eq!(display_name, "Test Server");
                assert_eq!(base_url, "https://chatgpt.com/backend-api");
                assert_eq!(environment_id, "env_123");
                assert_eq!(credential.account_id, "acct");
                assert_eq!(credential.access_token, "access-token");
            }
            other => panic!("expected slingshot reconnect plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_ssh_when_mode_is_ssh_and_credential() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("ssh".into());
        let cred = ssh_credential();
        let plan = compute_reconnect_plan(&s, Some(&cred), false);
        assert!(matches!(plan, Some(ReconnectPlan::Ssh { .. })));
    }

    #[test]
    fn plan_none_when_mode_is_ssh_but_no_credential() {
        let mut s = base_server();
        s.preferred_connection_mode = Some("ssh".into());
        assert!(compute_reconnect_plan(&s, None, false).is_none());
        assert!(matches!(
            decide_reconnect_plan_with_slingshot(&s, None, None, false),
            ReconnectPlanDecision::NoAction(ReconnectOutcome::MissingSshCredential)
        ));
    }

    #[test]
    fn plan_reports_missing_slingshot_credential() {
        let mut s = base_server();
        s.websocket_url = Some("slingshot://env_123?baseUrl=https://chatgpt.com".into());

        assert!(matches!(
            decide_reconnect_plan_with_slingshot(&s, None, None, false),
            ReconnectPlanDecision::NoAction(ReconnectOutcome::MissingSlingshotCredential)
        ));
    }

    #[test]
    fn plan_direct_remote_when_port_available() {
        let s = base_server();
        let plan = compute_reconnect_plan(&s, None, false);
        assert!(matches!(plan, Some(ReconnectPlan::DirectRemote { .. })));
        if let Some(ReconnectPlan::DirectRemote { port, .. }) = plan {
            assert_eq!(port, 8080);
        }
    }

    #[test]
    fn plan_ssh_legacy_fallback_when_no_mode_but_credential() {
        let mut s = base_server();
        // No explicit mode, no direct codex port available, but has credential
        s.has_codex_server = false;
        s.port = 0;
        s.codex_ports = vec![];
        let cred = ssh_credential();
        let plan = compute_reconnect_plan(&s, Some(&cred), false);
        assert!(matches!(plan, Some(ReconnectPlan::Ssh { .. })));
    }

    #[test]
    fn plan_local_when_source_is_local() {
        let mut s = base_server();
        s.source = "local".into();
        s.has_codex_server = false;
        s.port = 0;
        s.codex_ports = vec![];
        let plan = compute_reconnect_plan(&s, None, false);
        assert!(matches!(plan, Some(ReconnectPlan::Local { .. })));
    }

    #[test]
    fn plan_none_when_no_viable_transport() {
        let mut s = base_server();
        s.has_codex_server = false;
        s.port = 0;
        s.codex_ports = vec![];
        s.source = "manual".into();
        assert!(compute_reconnect_plan(&s, None, false).is_none());
    }

    #[test]
    fn historical_bridge_id_without_bridge_marker_preserves_direct_transport() {
        let mut s = base_server();
        s.id = "ssh-bridge:studio".into();
        s.has_codex_server = true;
        s.port = 8390;
        s.codex_ports = vec![8390];

        let plan = compute_reconnect_plan(&s, None, false);
        assert!(matches!(plan, Some(ReconnectPlan::DirectRemote { .. })));
    }

    #[test]
    fn historical_bridge_url_without_bridge_marker_preserves_explicit_ssh_transport() {
        let mut s = base_server();
        s.id = "ssh-bridge:studio".into();
        s.has_codex_server = false;
        s.port = 0;
        s.codex_ports = vec![];
        s.websocket_url = None;
        s.preferred_connection_mode = Some("ssh".into());
        let credential = ssh_credential();

        assert!(matches!(
            compute_reconnect_plan(&s, Some(&credential), false),
            Some(ReconnectPlan::Ssh { .. })
        ));
    }

    #[test]
    fn bridge_record_is_skipped_when_already_connected() {
        let mut s = base_server();
        s.ssh_bridge_runtime_kinds = Some(vec![]);
        assert!(compute_reconnect_plan(&s, None, true).is_none());
    }

    #[test]
    fn selected_ssh_bridge_runtimes_use_bridge_plan() {
        let mut s = base_server();
        s.id = "ssh-bridge:studio".into();
        s.hostname = "studio".into();
        s.port = 0;
        s.codex_ports = vec![];
        s.ssh_port = Some(22);
        s.preferred_connection_mode = Some("ssh".into());
        s.ssh_bridge_runtime_kinds = Some(vec![
            "pi".into(),
            "open-code".into(),
            "PI".into(),
            "unknown".into(),
        ]);
        let cred = ssh_credential();

        let plan = compute_reconnect_plan(&s, Some(&cred), false);

        match plan {
            Some(ReconnectPlan::SshBridge { runtime_kinds, .. }) => {
                assert_eq!(
                    runtime_kinds,
                    vec![
                        "pi".to_string(),
                        "opencode".to_string(),
                        "unknown".to_string()
                    ]
                );
            }
            other => panic!("expected ssh bridge reconnect plan, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_explicit_ssh_bridge_runtime_does_not_become_probe_all() {
        let mut s = base_server();
        s.id = "ssh-bridge:studio".into();
        s.hostname = "studio".into();
        s.port = 0;
        s.codex_ports = vec![];
        s.ssh_port = Some(22);
        s.preferred_connection_mode = Some("ssh".into());
        s.ssh_bridge_runtime_kinds = Some(vec!["droid".into()]);
        let cred = ssh_credential();

        let plan = compute_reconnect_plan(&s, Some(&cred), false);

        match plan {
            Some(ReconnectPlan::SshBridge { runtime_kinds, .. }) => {
                assert_eq!(runtime_kinds, vec!["droid".to_string()]);
                assert!(select_ssh_bridge_runtime_kinds(&runtime_kinds, &[]).is_err());
            }
            other => panic!("expected ssh bridge reconnect plan, got {other:?}"),
        }
    }

    #[test]
    fn empty_ssh_bridge_runtime_selection_preserves_probe_all_marker() {
        let mut s = base_server();
        s.id = "ssh-bridge:studio".into();
        s.hostname = "studio".into();
        s.port = 0;
        s.codex_ports = vec![];
        s.ssh_port = Some(22);
        s.preferred_connection_mode = Some("ssh".into());
        s.ssh_bridge_runtime_kinds = Some(vec![]);
        let cred = ssh_credential();

        let plan = compute_reconnect_plan(&s, Some(&cred), false);

        match plan {
            Some(ReconnectPlan::SshBridge { runtime_kinds, .. }) => {
                assert!(runtime_kinds.is_empty());
            }
            other => panic!("expected ssh bridge reconnect plan, got {other:?}"),
        }
    }

    #[test]
    fn ssh_bridge_without_credential_reports_missing_credential() {
        let mut s = base_server();
        s.ssh_bridge_runtime_kinds = Some(vec!["codex".into()]);

        assert!(matches!(
            decide_reconnect_plan_with_slingshot(&s, None, None, false),
            ReconnectPlanDecision::NoAction(ReconnectOutcome::MissingSshCredential)
        ));
    }
}
