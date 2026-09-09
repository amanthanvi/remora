use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    FullScopeLogin,
    LongLivedSetupToken,
    InferenceOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthContext {
    pub running_in_remote_environment: bool,
    pub managed_disable_remote_control: bool,
    pub provider_env_disables_remote_control: bool,
    pub token_kind: Option<TokenKind>,
    pub has_claude_ai_subscription: bool,
    pub organization_uuid: Option<String>,
    pub bridge_entitlement_enabled: bool,
    pub current_version: Option<String>,
    pub min_required_version: Option<String>,
    pub allow_remote_control_policy: bool,
    pub trusted_devices_required: bool,
    pub trusted_device_enrolled: bool,
    pub trusted_device_enrollment_temporarily_disabled: bool,
}

impl Default for AuthContext {
    fn default() -> Self {
        Self {
            running_in_remote_environment: false,
            managed_disable_remote_control: false,
            provider_env_disables_remote_control: false,
            token_kind: None,
            has_claude_ai_subscription: false,
            organization_uuid: None,
            bridge_entitlement_enabled: false,
            current_version: None,
            min_required_version: None,
            allow_remote_control_policy: true,
            trusted_devices_required: false,
            trusted_device_enrolled: false,
            trusted_device_enrollment_temporarily_disabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DisabledReason {
    RunningInRemoteSession,
    ManagedDisabled,
    ProviderEnvironment,
    MissingClaudeAiSubscription,
    FullScopeTokenRequired,
    MissingOrganization,
    EntitlementDisabled,
    VersionTooOld {
        version: String,
        min_version: String,
    },
    PolicyDenied,
    TrustedDeviceRequired,
    TrustedDeviceEnrollmentDisabled,
}

impl DisabledReason {
    pub fn message(&self) -> String {
        match self {
            Self::RunningInRemoteSession => {
                "Remote Control is not available inside a remote session.".to_string()
            }
            Self::ManagedDisabled => {
                "Remote Control is disabled by your organization's policy (managed setting `disableRemoteControl`).".to_string()
            }
            Self::ProviderEnvironment => {
                "Remote Control requires Claude.ai authentication and is unavailable for this provider environment.".to_string()
            }
            Self::MissingClaudeAiSubscription => {
                "Remote Control requires a claude.ai subscription. Run claude auth login to sign in with your claude.ai account.".to_string()
            }
            Self::FullScopeTokenRequired => {
                "Remote Control requires a full-scope login token. Long-lived tokens (from `claude setup-token` or CLAUDE_CODE_OAUTH_TOKEN) are limited to inference-only for security reasons. Run `claude auth login` to use Remote Control.".to_string()
            }
            Self::MissingOrganization => {
                "Unable to determine your organization for Remote Control eligibility. Run claude auth login to refresh your account information.".to_string()
            }
            Self::EntitlementDisabled => {
                "Remote Control is not yet enabled for your account.".to_string()
            }
            Self::VersionTooOld {
                version,
                min_version,
            } => format!(
                "Your version of Claude Code ({version}) is too old for Remote Control. Version {min_version} or higher is required. Run claude update to update."
            ),
            Self::PolicyDenied => {
                "Remote Control is disabled by your organization's policy.".to_string()
            }
            Self::TrustedDeviceRequired => {
                "Your organization requires Trusted Devices for Remote Control, but this device is not enrolled. Please run /login in Claude Code to enroll this device.".to_string()
            }
            Self::TrustedDeviceEnrollmentDisabled => {
                "Your organization requires Trusted Devices for Remote Control, but enrollment is temporarily disabled. Please try again later, or contact your administrator.".to_string()
            }
        }
    }
}

impl fmt::Display for DisabledReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteControlAvailability {
    pub available: bool,
    pub disabled_reason: Option<DisabledReason>,
}

impl RemoteControlAvailability {
    pub fn from_context(context: &AuthContext) -> Self {
        let disabled_reason = get_bridge_disabled_reason(context);
        Self {
            available: disabled_reason.is_none(),
            disabled_reason,
        }
    }
}

pub fn get_bridge_disabled_reason(context: &AuthContext) -> Option<DisabledReason> {
    if context.running_in_remote_environment {
        return Some(DisabledReason::RunningInRemoteSession);
    }
    if context.managed_disable_remote_control {
        return Some(DisabledReason::ManagedDisabled);
    }
    if context.provider_env_disables_remote_control {
        return Some(DisabledReason::ProviderEnvironment);
    }
    if !context.has_claude_ai_subscription || context.token_kind.is_none() {
        return Some(DisabledReason::MissingClaudeAiSubscription);
    }
    if context.token_kind != Some(TokenKind::FullScopeLogin) {
        return Some(DisabledReason::FullScopeTokenRequired);
    }
    if context
        .organization_uuid
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        return Some(DisabledReason::MissingOrganization);
    }
    if !context.bridge_entitlement_enabled {
        return Some(DisabledReason::EntitlementDisabled);
    }
    if let (Some(version), Some(min_version)) = (
        context.current_version.as_deref(),
        context.min_required_version.as_deref(),
    ) && !version_at_least(version, min_version)
    {
        return Some(DisabledReason::VersionTooOld {
            version: version.to_string(),
            min_version: min_version.to_string(),
        });
    }
    if !context.allow_remote_control_policy {
        return Some(DisabledReason::PolicyDenied);
    }
    if context.trusted_devices_required && !context.trusted_device_enrolled {
        if context.trusted_device_enrollment_temporarily_disabled {
            return Some(DisabledReason::TrustedDeviceEnrollmentDisabled);
        }
        return Some(DisabledReason::TrustedDeviceRequired);
    }
    None
}

fn version_at_least(version: &str, min_version: &str) -> bool {
    let version = numeric_version_segments(version);
    let min_version = numeric_version_segments(min_version);
    for idx in 0..version.len().max(min_version.len()) {
        let a = version.get(idx).copied().unwrap_or(0);
        let b = min_version.get(idx).copied().unwrap_or(0);
        match a.cmp(&b) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    true
}

fn numeric_version_segments(value: &str) -> Vec<u64> {
    value
        .split(|ch: char| !(ch.is_ascii_digit()))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available_context() -> AuthContext {
        AuthContext {
            token_kind: Some(TokenKind::FullScopeLogin),
            has_claude_ai_subscription: true,
            organization_uuid: Some("org".to_string()),
            bridge_entitlement_enabled: true,
            current_version: Some("2.1.136".to_string()),
            min_required_version: Some("2.1.0".to_string()),
            allow_remote_control_policy: true,
            ..AuthContext::default()
        }
    }

    #[test]
    fn rejects_setup_tokens_before_org_checks() {
        let mut ctx = available_context();
        ctx.token_kind = Some(TokenKind::LongLivedSetupToken);
        assert_eq!(
            get_bridge_disabled_reason(&ctx),
            Some(DisabledReason::FullScopeTokenRequired)
        );
    }

    #[test]
    fn rejects_versions_below_gate() {
        let mut ctx = available_context();
        ctx.current_version = Some("2.0.9".to_string());
        ctx.min_required_version = Some("2.1.0".to_string());
        assert_eq!(
            get_bridge_disabled_reason(&ctx),
            Some(DisabledReason::VersionTooOld {
                version: "2.0.9".to_string(),
                min_version: "2.1.0".to_string(),
            })
        );
    }

    #[test]
    fn accepts_full_scope_authorized_context() {
        assert_eq!(get_bridge_disabled_reason(&available_context()), None);
    }
}
