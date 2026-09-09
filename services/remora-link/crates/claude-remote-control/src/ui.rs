use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteControlReplState {
    pub repl_bridge_enabled: bool,
    pub repl_bridge_explicit: bool,
    pub repl_bridge_outbound_only: bool,
    pub repl_bridge_connected: bool,
    pub repl_bridge_session_active: bool,
    pub repl_bridge_reconnecting: bool,
    pub repl_bridge_error: Option<String>,
    pub repl_bridge_skip_next_archive: bool,
    pub repl_bridge_environment_id: Option<String>,
    pub repl_bridge_session_id: Option<String>,
    pub repl_bridge_session_url: Option<String>,
    pub repl_bridge_connect_url: Option<String>,
    pub repl_bridge_initial_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteControlStatus {
    Failed,
    Reconnecting,
    Active,
    Connecting,
}

impl RemoteControlReplState {
    pub fn status(&self) -> RemoteControlStatus {
        if self.repl_bridge_error.is_some() {
            RemoteControlStatus::Failed
        } else if self.repl_bridge_reconnecting {
            RemoteControlStatus::Reconnecting
        } else if self.repl_bridge_session_active || self.repl_bridge_connected {
            RemoteControlStatus::Active
        } else {
            RemoteControlStatus::Connecting
        }
    }
}

impl RemoteControlStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Failed => "Remote Control failed",
            Self::Reconnecting => "Remote Control reconnecting",
            Self::Active => "Remote Control active",
            Self::Connecting => "Remote Control connecting...",
        }
    }

    pub fn color(self) -> &'static str {
        match self {
            Self::Failed => "error",
            Self::Reconnecting | Self::Connecting => "warning",
            Self::Active => "success",
        }
    }
}

pub fn migrate_repl_bridge_setting(
    repl_bridge_enabled: Option<bool>,
    remote_control_at_startup: Option<bool>,
) -> Option<bool> {
    remote_control_at_startup.or(repl_bridge_enabled)
}

pub const REMOTE_CONTROL_ACTIVE_MESSAGE: &str = "/remote-control is active.";
pub const OUTBOUND_ONLY_REJECTION_MESSAGE: &str =
    "This session is outbound-only. Enable Remote Control locally to allow inbound control.";

pub const BRIDGE_LOG_TAGS: &[&str] = &[
    "[bridge:init]",
    "[bridge:api]",
    "[bridge:poll]",
    "[bridge:work]",
    "[bridge:session]",
    "[bridge:repl]",
    "[bridge:shutdown]",
    "[bridge:title]",
    "[bridge:sdk]",
    "[remote-bridge]",
    "[bridge-headless]",
    "[trusted-device]",
    "[SessionsV2Client]",
    "[SSETransport]",
    "[CCRClient]",
    "[RemoteSessionManager]",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_priority_matches_claude_header_logic() {
        let state = RemoteControlReplState {
            repl_bridge_connected: true,
            ..RemoteControlReplState::default()
        };
        assert_eq!(state.status(), RemoteControlStatus::Active);

        let state = RemoteControlReplState {
            repl_bridge_connected: true,
            repl_bridge_reconnecting: true,
            ..RemoteControlReplState::default()
        };
        assert_eq!(state.status(), RemoteControlStatus::Reconnecting);

        let state = RemoteControlReplState {
            repl_bridge_error: Some("boom".to_string()),
            repl_bridge_reconnecting: true,
            ..RemoteControlReplState::default()
        };
        assert_eq!(state.status(), RemoteControlStatus::Failed);
    }

    #[test]
    fn migration_preserves_existing_remote_control_setting_first() {
        assert_eq!(migrate_repl_bridge_setting(Some(true), None), Some(true));
        assert_eq!(
            migrate_repl_bridge_setting(Some(false), Some(true)),
            Some(true)
        );
    }
}
