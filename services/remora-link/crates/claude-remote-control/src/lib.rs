//! Reusable Claude Code Remote Control primitives.
//!
//! The crate mirrors the Remote Control surface documented in
//! `claude-code-remote-control.md`: CLI options, settings/preflight gates,
//! REST endpoints, work-loop records, session-ingress wire events, URL
//! builders, daemon config records, and a small high-level session manager.
//! It does not own Claude authentication or spawn the closed-source Claude
//! worker; callers provide OAuth/device tokens and decide where worker
//! processes run.

pub mod api;
pub mod auth;
pub mod cli;
pub mod config;
pub mod credentials;
pub mod daemon;
pub mod error;
pub mod manager;
pub mod runtime;
pub mod transport;
pub mod ui;
pub mod urls;
pub mod wire;

pub use api::{
    AuthRefreshCallback, BridgeApiClient, BridgeApiClientBuilder, BridgeAuth, BridgeCredential,
    RequestBeta,
};
pub use auth::{
    AuthContext, DisabledReason, RemoteControlAvailability, TokenKind, get_bridge_disabled_reason,
};
pub use cli::{RemoteControlArgs, SpawnMode};
pub use config::{
    ANTHROPIC_VERSION, CCR_BYOC_BETA, CLAUDE_CODE_ENVIRONMENT_KIND_BRIDGE, CLAUDE_CODE_REMOTE_TRUE,
    DEFAULT_RUNNER_VERSION, DEFAULT_USER_AGENT, ENVIRONMENTS_BETA, EndpointConfig, EnvConfig,
    EnvironmentKind, MANAGED_AGENTS_BETA, RemoteControlSettings, TuningConfig,
};
pub use credentials::{
    AccessTokenSource, CLAUDE_AI_INFERENCE_SCOPE, CLAUDE_AI_PROFILE_SCOPE,
    CLAUDE_AI_SESSIONS_SCOPE, CLAUDE_CODE_OAUTH_CLIENT_ID, ClaudeAiOauth, ClaudeAuthState,
    ClaudeConfigPaths, ClaudeCredentialStore, ClaudeCredentials, ClaudeGlobalConfig,
    CredentialBackend, OAuthAccount, TokenAccount, claude_code_keychain_service,
    read_api_key_override, read_session_ingress_token_override,
};
pub use daemon::{DaemonConfig, RemoteControlDaemonEntry};
pub use error::{BridgeApiError, BridgeApiErrorKind, RemoteControlError};
pub use manager::{RemoteSessionManager, RemoteSessionManagerBuilder};
pub use runtime::{
    ActiveRemoteSession, BridgeRuntime, BridgeRuntimeConfig, SessionSpawnContext, SessionSpawner,
    WorkLoopAction, worktree_name_for_session,
};
pub use transport::{
    SseDecoder, SseFrame, TransportKind, WebSocketJsonTransport, choose_transport_for_url,
};
pub use ui::{
    BRIDGE_LOG_TAGS, RemoteControlReplState, RemoteControlStatus, migrate_repl_bridge_setting,
};
pub use urls::{environment_connect_url, session_ingress_ws_url, session_url};
pub use wire::{
    BridgeEnvironmentRegistration, BridgeEnvironmentRegistrationResponse, ControlRequest,
    ControlRequestEnvelope, ControlResponse, GroveAccountSettings, PermissionBehavior,
    PermissionMode, PermissionRequestEvent, PermissionResponseEvent, ReconnectSessionResponse,
    RemoteEvent, SessionContext, SessionCreateRequest, SessionEventsPage, SessionRecord,
    SessionSource, SessionsPage, Work, WorkData, WorkHeartbeatResponse, WorkSecret,
    WorkerEventsPage, WorkerStateResponse,
};
