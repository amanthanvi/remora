//! Handwritten, bounded UniFFI contract for command-center capability state.

use std::collections::HashMap;

use crate::store::boundary::session_summaries_from_snapshot;
use crate::store::{AppSessionSummary, AppSnapshot};
use crate::types::{ThreadKey, ThreadSummaryStatus};
use remora_bridge_core::command_center::{
    AvailabilityState as LinkAvailabilityState, FeatureAvailability as LinkFeatureAvailability,
    HostCapabilitiesV1 as LinkHostCapabilitiesV1,
    HostCommandCenterStatusV1 as LinkHostCommandCenterStatusV1,
    ProviderInstance as LinkProviderInstance, ProviderReadiness as LinkProviderReadiness,
    RuntimeCapabilitiesV1 as LinkRuntimeCapabilitiesV1,
};

const MAX_REASON_BYTES: usize = 256;
const MAX_COMMAND_CENTER_HOSTS: usize = 32;
const MAX_SESSION_PAGE_ROWS: usize = 100;
const MAX_MISSION_LANE_ROWS: usize = 20;
const MAX_TITLE_BYTES: usize = 256;
const MAX_PREVIEW_BYTES: usize = 512;
const MAX_RUNTIME_BYTES: usize = 64;
const MAX_MODEL_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AvailabilityState {
    Available,
    Unknown,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FeatureAvailability {
    pub state: AvailabilityState,
    pub reason: Option<String>,
}

impl FeatureAvailability {
    fn available() -> Self {
        Self {
            state: AvailabilityState::Available,
            reason: None,
        }
    }

    fn unknown(reason: &str) -> Self {
        Self {
            state: AvailabilityState::Unknown,
            reason: Some(bound_utf8(reason, MAX_REASON_BYTES)),
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            state: AvailabilityState::Unavailable,
            reason: Some(bound_utf8(reason, MAX_REASON_BYTES)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadLifecycleCapabilitiesV1 {
    pub create: FeatureAvailability,
    pub resume: FeatureAvailability,
    pub linked_child: FeatureAvailability,
    pub archive: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TurnCapabilitiesV1 {
    pub text: FeatureAvailability,
    pub images: FeatureAvailability,
    pub file_references: FeatureAvailability,
    pub interrupt: FeatureAvailability,
    pub queued_follow_up: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InteractionCapabilitiesV1 {
    pub approvals: FeatureAvailability,
    pub structured_input: FeatureAvailability,
    pub ask_question: FeatureAvailability,
    pub plans: FeatureAvailability,
    pub todos: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelCapabilitiesV1 {
    pub list: FeatureAvailability,
    pub select_before_first_send: FeatureAvailability,
    pub reasoning_configuration: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PermissionCapabilitiesV1 {
    pub sandbox_modes: FeatureAvailability,
    pub declared_controls: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HistoryCapabilitiesV1 {
    pub pagination: FeatureAvailability,
    pub hydration: FeatureAvailability,
    pub context_window_metrics: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VoiceCapabilitiesV1 {
    pub realtime_voice: FeatureAvailability,
    pub transcript_handoff: FeatureAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RuntimeCapabilitiesV1 {
    pub version: u32,
    pub thread_lifecycle: ThreadLifecycleCapabilitiesV1,
    pub turns: TurnCapabilitiesV1,
    pub interaction: InteractionCapabilitiesV1,
    pub models: ModelCapabilitiesV1,
    pub permissions: PermissionCapabilitiesV1,
    pub history: HistoryCapabilitiesV1,
    pub voice: VoiceCapabilitiesV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProviderReadiness {
    Ready,
    AuthenticationRequired,
    InstallationRequired,
    ConfigurationRequired,
    Starting,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelDescriptor {
    pub model_id: String,
    pub display_name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProviderInstance {
    pub instance_id: String,
    pub runtime_id: String,
    pub display_name: String,
    pub readiness: ProviderReadiness,
    pub readiness_reason: Option<String>,
    pub continuation_group_id: String,
    pub models: Vec<ModelDescriptor>,
    pub capabilities: RuntimeCapabilitiesV1,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HostCapabilitiesV1 {
    pub version: u32,
    pub project_registration: FeatureAvailability,
    pub project_clone: FeatureAvailability,
    pub confined_file_reads: FeatureAvailability,
    pub terminal_sessions: FeatureAvailability,
    pub curated_git: FeatureAvailability,
    pub worktrees: FeatureAvailability,
    pub checkpoints: FeatureAvailability,
    pub safe_rewind: FeatureAvailability,
    pub trusted_provisioning_scripts: FeatureAvailability,
    pub browser_preview: FeatureAvailability,
    pub browser_automation: FeatureAvailability,
    pub managed_power: FeatureAvailability,
    pub signed_link_updates: FeatureAvailability,
    pub diagnostics: FeatureAvailability,
    pub protocol_version: u32,
    pub minimum_client_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HostCommandCenterStatus {
    /// Transitional store key only. It is not a durable Host ID.
    pub legacy_server_id: String,
    pub host_id: Option<String>,
    pub catalog_generation: Option<u64>,
    pub availability: FeatureAvailability,
    pub capabilities: HostCapabilitiesV1,
    pub provider_instances: Vec<ProviderInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandCenterStatusV1 {
    pub schema_version: u32,
    pub hosts: Vec<HostCommandCenterStatus>,
    pub overflow_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NewTaskLaunchAvailabilityV1 {
    pub can_launch: bool,
    pub availability: FeatureAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, uniffi::Enum)]
pub enum SessionStatusV1 {
    Idle,
    Running,
    Waiting,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, uniffi::Enum)]
pub enum SessionAttentionV1 {
    None,
    NeedsYou,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, uniffi::Record)]
pub struct SessionListRowV1 {
    /// Transitional app-server key. Durable command-center routes use the
    /// opaque Link-owned IDs once negotiated.
    pub key: ThreadKey,
    pub host_label: String,
    pub project_label: Option<String>,
    pub runtime_id: String,
    pub model_label: Option<String>,
    pub title: String,
    pub preview: Option<String>,
    pub status: SessionStatusV1,
    pub attention: SessionAttentionV1,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, uniffi::Record)]
pub struct SessionPageV1 {
    pub rows: Vec<SessionListRowV1>,
    pub next_cursor: Option<String>,
    pub total_count: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, uniffi::Record)]
pub struct MissionControlProjectionV1 {
    pub needs_you: Vec<SessionListRowV1>,
    pub active: Vec<SessionListRowV1>,
    pub recent: Vec<SessionListRowV1>,
    pub needs_you_count: u32,
    pub active_count: u32,
    pub recent_count: u32,
}

pub(crate) fn project_command_center_status(
    snapshot: &AppSnapshot,
    link_statuses: &HashMap<String, LinkHostCommandCenterStatusV1>,
) -> CommandCenterStatusV1 {
    let mut servers = snapshot.servers.values().collect::<Vec<_>>();
    servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
    let overflow_count = servers.len().saturating_sub(MAX_COMMAND_CENTER_HOSTS) as u32;
    servers.truncate(MAX_COMMAND_CENTER_HOSTS);
    let hosts = servers
        .into_iter()
        .map(|server| {
            if let Some(status) = link_statuses.get(&server.server_id) {
                return project_link_command_center_status(&server.server_id, status.clone());
            }
            let is_link = server.server_id.starts_with("remora-link:");
            let reason = if is_link {
                "Connected Link protocol has not declared command-center v1"
            } else {
                "Command-center projects require a trusted Remora Link Host"
            };
            let availability = if is_link {
                FeatureAvailability::unknown(reason)
            } else {
                FeatureAvailability::unavailable(reason)
            };
            let feature = || availability.clone();
            HostCommandCenterStatus {
                legacy_server_id: bound_utf8(&server.server_id, MAX_REASON_BYTES),
                host_id: None,
                catalog_generation: None,
                availability: availability.clone(),
                capabilities: HostCapabilitiesV1 {
                    version: 1,
                    project_registration: feature(),
                    project_clone: feature(),
                    confined_file_reads: feature(),
                    terminal_sessions: feature(),
                    curated_git: feature(),
                    worktrees: feature(),
                    checkpoints: feature(),
                    safe_rewind: feature(),
                    trusted_provisioning_scripts: feature(),
                    browser_preview: feature(),
                    browser_automation: feature(),
                    managed_power: feature(),
                    signed_link_updates: feature(),
                    diagnostics: feature(),
                    protocol_version: 0,
                    minimum_client_version: String::new(),
                },
                // A legacy runtime name is not a durable named instance.
                provider_instances: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    CommandCenterStatusV1 {
        schema_version: 1,
        hosts,
        overflow_count,
    }
}

pub(crate) fn project_new_task_launch_availability(
    snapshot: &AppSnapshot,
    link_statuses: &HashMap<String, LinkHostCommandCenterStatusV1>,
    server_id: &str,
    runtime_id: Option<&str>,
) -> NewTaskLaunchAvailabilityV1 {
    let Some(server) = snapshot.servers.get(server_id) else {
        return launch_unavailable("This Host is no longer available");
    };
    if !matches!(server.health, crate::store::ServerHealthSnapshot::Connected) {
        return launch_unavailable("Reconnect this Host before starting work");
    }

    let runtime_id = runtime_id.map(str::trim).filter(|value| !value.is_empty());
    if let Some(status) = link_statuses.get(server_id) {
        let mut providers = status
            .provider_instances
            .iter()
            .filter(|provider| runtime_id.is_none_or(|selected| provider.runtime_id == selected))
            .collect::<Vec<_>>();
        providers.sort_by(|left, right| left.instance_id.as_str().cmp(right.instance_id.as_str()));

        if providers
            .iter()
            .any(|provider| matches!(provider.readiness, LinkProviderReadiness::Ready))
        {
            return launch_available();
        }
        if providers.is_empty() {
            return launch_unavailable(if runtime_id.is_some() {
                "The selected provider is not available on this Host"
            } else {
                "No coding provider is available on this Host"
            });
        }
        let provider = providers[0];
        let reason = provider
            .readiness_reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or_else(|| provider_readiness_reason(provider.readiness));
        return launch_unavailable(reason);
    }

    let matching_runtimes = server
        .agent_runtimes
        .iter()
        .filter(|runtime| runtime_id.is_none_or(|selected| runtime.kind == selected))
        .collect::<Vec<_>>();
    if matching_runtimes.iter().any(|runtime| runtime.available) {
        launch_available()
    } else if matching_runtimes.is_empty() {
        launch_unavailable(if runtime_id.is_some() {
            "The selected provider is not connected to this Host"
        } else {
            "No coding provider is connected to this Host"
        })
    } else {
        launch_unavailable("The selected provider is unavailable on this Host")
    }
}

fn launch_available() -> NewTaskLaunchAvailabilityV1 {
    NewTaskLaunchAvailabilityV1 {
        can_launch: true,
        availability: FeatureAvailability::available(),
    }
}

fn launch_unavailable(reason: &str) -> NewTaskLaunchAvailabilityV1 {
    NewTaskLaunchAvailabilityV1 {
        can_launch: false,
        availability: FeatureAvailability::unavailable(reason),
    }
}

fn provider_readiness_reason(readiness: LinkProviderReadiness) -> &'static str {
    match readiness {
        LinkProviderReadiness::Ready => "Provider is ready",
        LinkProviderReadiness::AuthenticationRequired => {
            "Authenticate this provider on the Host before starting work"
        }
        LinkProviderReadiness::InstallationRequired => {
            "Install this provider on the Host before starting work"
        }
        LinkProviderReadiness::ConfigurationRequired => {
            "Configure this provider on the Host before starting work"
        }
        LinkProviderReadiness::Starting => "This provider is still starting on the Host",
        LinkProviderReadiness::Unavailable => "This provider is unavailable on the Host",
    }
}

pub(crate) fn project_link_command_center_status(
    legacy_server_id: &str,
    status: LinkHostCommandCenterStatusV1,
) -> HostCommandCenterStatus {
    HostCommandCenterStatus {
        legacy_server_id: bound_utf8(legacy_server_id, MAX_REASON_BYTES),
        host_id: Some(status.host_id.as_str().to_string()),
        catalog_generation: Some(status.catalog_generation),
        availability: FeatureAvailability::available(),
        capabilities: project_link_host_capabilities(status.host_capabilities),
        provider_instances: status
            .provider_instances
            .into_iter()
            .map(project_link_provider_instance)
            .collect(),
    }
}

pub(crate) fn project_unknown_link_command_center_status(
    legacy_server_id: &str,
) -> HostCommandCenterStatus {
    let availability =
        FeatureAvailability::unknown("Connected Link protocol has not declared command-center v1");
    let feature = || availability.clone();
    HostCommandCenterStatus {
        legacy_server_id: bound_utf8(legacy_server_id, MAX_REASON_BYTES),
        host_id: None,
        catalog_generation: None,
        availability: availability.clone(),
        capabilities: HostCapabilitiesV1 {
            version: 1,
            project_registration: feature(),
            project_clone: feature(),
            confined_file_reads: feature(),
            terminal_sessions: feature(),
            curated_git: feature(),
            worktrees: feature(),
            checkpoints: feature(),
            safe_rewind: feature(),
            trusted_provisioning_scripts: feature(),
            browser_preview: feature(),
            browser_automation: feature(),
            managed_power: feature(),
            signed_link_updates: feature(),
            diagnostics: feature(),
            protocol_version: 0,
            minimum_client_version: String::new(),
        },
        provider_instances: Vec::new(),
    }
}

fn project_link_feature(value: LinkFeatureAvailability) -> FeatureAvailability {
    FeatureAvailability {
        state: match value.state {
            LinkAvailabilityState::Available => AvailabilityState::Available,
            LinkAvailabilityState::Unknown => AvailabilityState::Unknown,
            LinkAvailabilityState::Unavailable => AvailabilityState::Unavailable,
        },
        reason: value
            .reason
            .map(|reason| bound_utf8(&reason, MAX_REASON_BYTES)),
    }
}

fn project_link_host_capabilities(value: LinkHostCapabilitiesV1) -> HostCapabilitiesV1 {
    HostCapabilitiesV1 {
        version: value.version,
        project_registration: project_link_feature(value.project_registration),
        project_clone: project_link_feature(value.project_clone),
        confined_file_reads: project_link_feature(value.confined_file_reads),
        terminal_sessions: project_link_feature(value.terminal_sessions),
        curated_git: project_link_feature(value.curated_git),
        worktrees: project_link_feature(value.worktrees),
        checkpoints: project_link_feature(value.checkpoints),
        safe_rewind: project_link_feature(value.safe_rewind),
        trusted_provisioning_scripts: project_link_feature(value.trusted_provisioning_scripts),
        browser_preview: project_link_feature(value.browser_preview),
        browser_automation: project_link_feature(value.browser_automation),
        managed_power: project_link_feature(value.managed_power),
        signed_link_updates: project_link_feature(value.signed_link_updates),
        diagnostics: project_link_feature(value.diagnostics),
        protocol_version: value.protocol_version,
        minimum_client_version: bound_utf8(&value.minimum_client_version, MAX_REASON_BYTES),
    }
}

fn project_link_provider_instance(value: LinkProviderInstance) -> ProviderInstance {
    ProviderInstance {
        instance_id: value.instance_id.as_str().to_string(),
        runtime_id: bound_utf8(&value.runtime_id, MAX_RUNTIME_BYTES),
        display_name: bound_utf8(&value.display_name, MAX_TITLE_BYTES),
        readiness: match value.readiness {
            LinkProviderReadiness::Ready => ProviderReadiness::Ready,
            LinkProviderReadiness::AuthenticationRequired => {
                ProviderReadiness::AuthenticationRequired
            }
            LinkProviderReadiness::InstallationRequired => ProviderReadiness::InstallationRequired,
            LinkProviderReadiness::ConfigurationRequired => {
                ProviderReadiness::ConfigurationRequired
            }
            LinkProviderReadiness::Starting => ProviderReadiness::Starting,
            LinkProviderReadiness::Unavailable => ProviderReadiness::Unavailable,
        },
        readiness_reason: value
            .readiness_reason
            .map(|reason| bound_utf8(&reason, MAX_REASON_BYTES)),
        continuation_group_id: bound_utf8(&value.continuation_group_id, MAX_TITLE_BYTES),
        models: value
            .models
            .into_iter()
            .map(|model| ModelDescriptor {
                model_id: bound_utf8(&model.model_id, MAX_TITLE_BYTES),
                display_name: bound_utf8(&model.display_name, MAX_TITLE_BYTES),
                is_default: model.is_default,
            })
            .collect(),
        capabilities: project_link_runtime_capabilities(value.capabilities),
    }
}

fn project_link_runtime_capabilities(value: LinkRuntimeCapabilitiesV1) -> RuntimeCapabilitiesV1 {
    RuntimeCapabilitiesV1 {
        version: value.version,
        thread_lifecycle: ThreadLifecycleCapabilitiesV1 {
            create: project_link_feature(value.thread_lifecycle.create),
            resume: project_link_feature(value.thread_lifecycle.resume),
            linked_child: project_link_feature(value.thread_lifecycle.linked_child),
            archive: project_link_feature(value.thread_lifecycle.archive),
        },
        turns: TurnCapabilitiesV1 {
            text: project_link_feature(value.turns.text),
            images: project_link_feature(value.turns.images),
            file_references: project_link_feature(value.turns.file_references),
            interrupt: project_link_feature(value.turns.interrupt),
            queued_follow_up: project_link_feature(value.turns.queued_follow_up),
        },
        interaction: InteractionCapabilitiesV1 {
            approvals: project_link_feature(value.interaction.approvals),
            structured_input: project_link_feature(value.interaction.structured_input),
            ask_question: project_link_feature(value.interaction.ask_question),
            plans: project_link_feature(value.interaction.plans),
            todos: project_link_feature(value.interaction.todos),
        },
        models: ModelCapabilitiesV1 {
            list: project_link_feature(value.models.list),
            select_before_first_send: project_link_feature(value.models.select_before_first_send),
            reasoning_configuration: project_link_feature(value.models.reasoning_configuration),
        },
        permissions: PermissionCapabilitiesV1 {
            sandbox_modes: project_link_feature(value.permissions.sandbox_modes),
            declared_controls: project_link_feature(value.permissions.declared_controls),
        },
        history: HistoryCapabilitiesV1 {
            pagination: project_link_feature(value.history.pagination),
            hydration: project_link_feature(value.history.hydration),
            context_window_metrics: project_link_feature(value.history.context_window_metrics),
        },
        voice: VoiceCapabilitiesV1 {
            realtime_voice: project_link_feature(value.voice.realtime_voice),
            transcript_handoff: project_link_feature(value.voice.transcript_handoff),
        },
    }
}

pub(crate) fn project_sessions_page(
    snapshot: &AppSnapshot,
    cursor: Option<&str>,
    limit: Option<u32>,
) -> Result<SessionPageV1, String> {
    let summaries = session_summaries_from_snapshot(snapshot);
    let start = decode_session_cursor(cursor, summaries.len())?;
    let limit = limit
        .unwrap_or(MAX_SESSION_PAGE_ROWS as u32)
        .clamp(1, MAX_SESSION_PAGE_ROWS as u32) as usize;
    let end = start.saturating_add(limit).min(summaries.len());
    let rows = summaries[start..end]
        .iter()
        .map(|summary| session_row(snapshot, summary))
        .collect();
    Ok(SessionPageV1 {
        rows,
        next_cursor: (end < summaries.len()).then(|| format!("v1:{end}")),
        total_count: summaries.len().min(u32::MAX as usize) as u32,
    })
}

pub(crate) fn project_mission_control(snapshot: &AppSnapshot) -> MissionControlProjectionV1 {
    let summaries = session_summaries_from_snapshot(snapshot);
    let rows = summaries
        .iter()
        .map(|summary| session_row(snapshot, summary))
        .collect::<Vec<_>>();
    let needs_you_count = rows
        .iter()
        .filter(|row| row.attention == SessionAttentionV1::NeedsYou)
        .count();
    let active_count = rows
        .iter()
        .filter(|row| row.status == SessionStatusV1::Running)
        .count();
    let recent_count = rows
        .len()
        .saturating_sub(needs_you_count)
        .saturating_sub(active_count);

    MissionControlProjectionV1 {
        needs_you: rows
            .iter()
            .filter(|row| row.attention == SessionAttentionV1::NeedsYou)
            .take(MAX_MISSION_LANE_ROWS)
            .cloned()
            .collect(),
        active: rows
            .iter()
            .filter(|row| row.status == SessionStatusV1::Running)
            .take(MAX_MISSION_LANE_ROWS)
            .cloned()
            .collect(),
        recent: rows
            .iter()
            .filter(|row| {
                row.attention != SessionAttentionV1::NeedsYou
                    && row.status != SessionStatusV1::Running
            })
            .take(MAX_MISSION_LANE_ROWS)
            .cloned()
            .collect(),
        needs_you_count: needs_you_count.min(u32::MAX as usize) as u32,
        active_count: active_count.min(u32::MAX as usize) as u32,
        recent_count: recent_count.min(u32::MAX as usize) as u32,
    }
}

fn decode_session_cursor(cursor: Option<&str>, total: usize) -> Result<usize, String> {
    let Some(cursor) = cursor else { return Ok(0) };
    if cursor.len() > 32 {
        return Err("invalid Sessions cursor".to_string());
    }
    let offset = cursor
        .strip_prefix("v1:")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|offset| *offset <= total)
        .ok_or_else(|| "invalid Sessions cursor".to_string())?;
    Ok(offset)
}

fn session_row(snapshot: &AppSnapshot, summary: &AppSessionSummary) -> SessionListRowV1 {
    let attention = has_pending_attention(snapshot, &summary.key);
    let status = if attention {
        SessionStatusV1::Waiting
    } else if summary.has_active_turn {
        SessionStatusV1::Running
    } else {
        snapshot
            .threads
            .get(&summary.key)
            .map_or(SessionStatusV1::Unknown, |thread| {
                match &thread.info.status {
                    ThreadSummaryStatus::SystemError => SessionStatusV1::Failed,
                    ThreadSummaryStatus::Active => SessionStatusV1::Running,
                    ThreadSummaryStatus::Idle => SessionStatusV1::Idle,
                    ThreadSummaryStatus::NotLoaded => SessionStatusV1::Unknown,
                }
            })
    };
    SessionListRowV1 {
        key: summary.key.clone(),
        host_label: bound_utf8(&summary.server_display_name, MAX_TITLE_BYTES),
        project_label: project_label(&summary.cwd),
        runtime_id: bound_utf8(&summary.agent_runtime_kind, MAX_RUNTIME_BYTES),
        model_label: nonempty_bounded(&summary.model, MAX_MODEL_BYTES),
        title: bound_utf8(&summary.title, MAX_TITLE_BYTES),
        preview: summary
            .last_response_preview
            .as_deref()
            .or_else(|| (!summary.preview.is_empty()).then_some(summary.preview.as_str()))
            .map(|value| bound_utf8(value, MAX_PREVIEW_BYTES)),
        status,
        attention: if attention {
            SessionAttentionV1::NeedsYou
        } else {
            SessionAttentionV1::None
        },
        updated_at_ms: summary
            .updated_at
            .and_then(|seconds| seconds.checked_mul(1_000)),
    }
}

fn has_pending_attention(snapshot: &AppSnapshot, key: &ThreadKey) -> bool {
    snapshot.pending_approvals.iter().any(|approval| {
        approval.server_id == key.server_id
            && approval.thread_id.as_deref() == Some(key.thread_id.as_str())
    }) || snapshot
        .pending_user_inputs
        .iter()
        .any(|request| request.server_id == key.server_id && request.thread_id == key.thread_id)
}

fn project_label(cwd: &str) -> Option<String> {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .map(|segment| bound_utf8(segment, MAX_TITLE_BYTES))
}

fn nonempty_bounded(value: &str, maximum: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| bound_utf8(value, maximum))
}

fn bound_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::ServerConfig;
    use crate::store::{AppStoreReducer, ServerHealthSnapshot};
    use crate::types::ThreadInfo;

    fn server_config(server_id: &str) -> ServerConfig {
        ServerConfig {
            server_id: server_id.to_string(),
            display_name: "Host".to_string(),
            host: server_id.to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        }
    }

    fn link_provider(
        instance_id: &str,
        runtime_id: &str,
        readiness: LinkProviderReadiness,
        readiness_reason: Option<String>,
    ) -> LinkProviderInstance {
        LinkProviderInstance {
            instance_id: remora_bridge_core::command_center::ProviderInstanceId(
                instance_id.to_string(),
            ),
            runtime_id: runtime_id.to_string(),
            display_name: runtime_id.to_string(),
            readiness,
            readiness_reason,
            continuation_group_id: runtime_id.to_string(),
            models: Vec::new(),
            capabilities: LinkRuntimeCapabilitiesV1::all_unknown(),
        }
    }

    fn link_status(providers: Vec<LinkProviderInstance>) -> LinkHostCommandCenterStatusV1 {
        LinkHostCommandCenterStatusV1 {
            version: 1,
            host_id: remora_bridge_core::command_center::HostId(
                "AQEBAQEBAQEBAQEBAQEBAQ".to_string(),
            ),
            catalog_generation: 7,
            host_capabilities: LinkHostCapabilitiesV1::all_unknown(2, "0.1.0"),
            provider_instances: providers,
        }
    }

    fn thread_info(index: usize) -> ThreadInfo {
        ThreadInfo {
            id: format!("thread-{index:03}"),
            title: Some("T".repeat(1_000)),
            model: Some("M".repeat(1_000)),
            status: ThreadSummaryStatus::Idle,
            preview: Some("P".repeat(1_000)),
            cwd: Some(format!("/tmp/project-{index:03}")),
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: Some(index as i64),
        }
    }

    #[test]
    fn old_link_degrades_to_unknown_without_inventing_provider_instances() {
        let store = AppStoreReducer::new();
        store.upsert_server(
            &ServerConfig {
                server_id: "remora-link:legacy-node".to_string(),
                display_name: "Host".to_string(),
                host: "legacy-node".to_string(),
                port: 0,
                websocket_url: None,
                is_local: false,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        let projection = project_command_center_status(&store.snapshot(), &HashMap::new());
        assert_eq!(projection.hosts.len(), 1);
        assert_eq!(projection.overflow_count, 0);
        assert_eq!(
            projection.hosts[0].availability.state,
            AvailabilityState::Unknown
        );
        assert!(projection.hosts[0].provider_instances.is_empty());
    }

    #[test]
    fn raw_app_server_is_unavailable_for_command_center_projects() {
        let store = AppStoreReducer::new();
        store.upsert_server(
            &ServerConfig {
                server_id: "manual-server".to_string(),
                display_name: "Manual".to_string(),
                host: "localhost".to_string(),
                port: 8080,
                websocket_url: None,
                is_local: false,
                tls: false,
            },
            ServerHealthSnapshot::Disconnected,
        );

        let projection = project_command_center_status(&store.snapshot(), &HashMap::new());
        assert_eq!(
            projection.hosts[0].availability.state,
            AvailabilityState::Unavailable
        );
    }

    #[test]
    fn authenticated_link_status_replaces_unknown_for_exact_server_only() {
        let store = AppStoreReducer::new();
        for server_id in ["remora-link:node-a", "remora-link:node-b"] {
            store.upsert_server(
                &ServerConfig {
                    server_id: server_id.to_string(),
                    display_name: "Host".to_string(),
                    host: server_id.to_string(),
                    port: 0,
                    websocket_url: None,
                    is_local: false,
                    tls: false,
                },
                ServerHealthSnapshot::Connected,
            );
        }
        let status = LinkHostCommandCenterStatusV1 {
            version: 1,
            host_id: remora_bridge_core::command_center::HostId(
                "AQEBAQEBAQEBAQEBAQEBAQ".to_string(),
            ),
            catalog_generation: 7,
            host_capabilities: LinkHostCapabilitiesV1::all_unknown(2, "0.1.0"),
            provider_instances: Vec::new(),
        };
        let statuses = HashMap::from([("remora-link:node-b".to_string(), status)]);

        let projection = project_command_center_status(&store.snapshot(), &statuses);

        assert_eq!(projection.hosts.len(), 2);
        assert_eq!(
            projection.hosts[0].availability.state,
            AvailabilityState::Unknown
        );
        assert_eq!(
            projection.hosts[1].availability.state,
            AvailabilityState::Available
        );
        assert_eq!(projection.hosts[1].catalog_generation, Some(7));
        assert_eq!(
            projection.hosts[1].host_id.as_deref(),
            Some("AQEBAQEBAQEBAQEBAQEBAQ")
        );
    }

    #[test]
    fn command_center_hosts_are_sorted_clamped_and_count_overflow() {
        let store = AppStoreReducer::new();
        for index in (0..40).rev() {
            let server_id = format!("remora-link:node-{index:02}");
            store.upsert_server(
                &ServerConfig {
                    server_id: server_id.clone(),
                    display_name: server_id.clone(),
                    host: server_id,
                    port: 0,
                    websocket_url: None,
                    is_local: false,
                    tls: false,
                },
                ServerHealthSnapshot::Connected,
            );
        }

        let projection = project_command_center_status(&store.snapshot(), &HashMap::new());

        assert_eq!(projection.hosts.len(), MAX_COMMAND_CENTER_HOSTS);
        assert_eq!(projection.overflow_count, 8);
        assert_eq!(projection.hosts[0].legacy_server_id, "remora-link:node-00");
        assert_eq!(projection.hosts[31].legacy_server_id, "remora-link:node-31");
    }

    #[test]
    fn new_task_launch_uses_connected_runtime_directory_for_legacy_hosts() {
        let server_id = "remora-link:legacy-node";
        let store = AppStoreReducer::new();
        store.upsert_server(&server_config(server_id), ServerHealthSnapshot::Connected);

        let available = project_new_task_launch_availability(
            &store.snapshot(),
            &HashMap::new(),
            server_id,
            Some("codex"),
        );
        assert!(available.can_launch);
        assert_eq!(available.availability.state, AvailabilityState::Available);

        let missing = project_new_task_launch_availability(
            &store.snapshot(),
            &HashMap::new(),
            server_id,
            Some("claude"),
        );
        assert!(!missing.can_launch);
        assert_eq!(missing.availability.state, AvailabilityState::Unavailable);
    }

    #[test]
    fn new_task_launch_requires_a_connected_known_host() {
        let server_id = "remora-link:offline-node";
        let store = AppStoreReducer::new();
        store.upsert_server(
            &server_config(server_id),
            ServerHealthSnapshot::Disconnected,
        );

        let offline = project_new_task_launch_availability(
            &store.snapshot(),
            &HashMap::new(),
            server_id,
            None,
        );
        assert!(!offline.can_launch);
        assert_eq!(
            offline.availability.reason.as_deref(),
            Some("Reconnect this Host before starting work")
        );

        let removed = project_new_task_launch_availability(
            &store.snapshot(),
            &HashMap::new(),
            "missing",
            None,
        );
        assert!(!removed.can_launch);
        assert_eq!(
            removed.availability.reason.as_deref(),
            Some("This Host is no longer available")
        );
    }

    #[test]
    fn new_task_launch_obeys_authoritative_provider_readiness_and_reason() {
        let server_id = "remora-link:node";
        let store = AppStoreReducer::new();
        store.upsert_server(&server_config(server_id), ServerHealthSnapshot::Connected);
        let statuses = HashMap::from([(
            server_id.to_string(),
            link_status(vec![
                link_provider(
                    "provider-b",
                    "codex",
                    LinkProviderReadiness::AuthenticationRequired,
                    Some("Sign in on the Host".to_string()),
                ),
                link_provider(
                    "provider-a",
                    "codex",
                    LinkProviderReadiness::Starting,
                    Some("Host runtime is warming up".to_string()),
                ),
                link_provider("provider-c", "claude", LinkProviderReadiness::Ready, None),
            ]),
        )]);

        let blocked = project_new_task_launch_availability(
            &store.snapshot(),
            &statuses,
            server_id,
            Some("codex"),
        );
        assert!(!blocked.can_launch);
        assert_eq!(
            blocked.availability.reason.as_deref(),
            Some("Host runtime is warming up")
        );

        let ready = project_new_task_launch_availability(
            &store.snapshot(),
            &statuses,
            server_id,
            Some("claude"),
        );
        assert!(ready.can_launch);

        let missing = project_new_task_launch_availability(
            &store.snapshot(),
            &statuses,
            server_id,
            Some("cursor"),
        );
        assert!(!missing.can_launch);
        assert_eq!(
            missing.availability.reason.as_deref(),
            Some("The selected provider is not available on this Host")
        );
    }

    #[test]
    fn new_task_launch_bounds_host_guidance_and_has_readiness_fallbacks() {
        let server_id = "remora-link:node";
        let store = AppStoreReducer::new();
        store.upsert_server(&server_config(server_id), ServerHealthSnapshot::Connected);
        let statuses = HashMap::from([(
            server_id.to_string(),
            link_status(vec![link_provider(
                "provider-a",
                "codex",
                LinkProviderReadiness::ConfigurationRequired,
                Some("🦀".repeat(100)),
            )]),
        )]);

        let bounded = project_new_task_launch_availability(
            &store.snapshot(),
            &statuses,
            server_id,
            Some("codex"),
        );
        let reason = bounded.availability.reason.expect("reason");
        assert!(!bounded.can_launch);
        assert!(reason.len() <= MAX_REASON_BYTES);
        assert!(reason.is_char_boundary(reason.len()));

        let starting_statuses = HashMap::from([(
            server_id.to_string(),
            link_status(vec![link_provider(
                "provider-a",
                "codex",
                LinkProviderReadiness::Starting,
                None,
            )]),
        )]);
        let starting = project_new_task_launch_availability(
            &store.snapshot(),
            &starting_statuses,
            server_id,
            Some("codex"),
        );
        assert_eq!(
            starting.availability.reason.as_deref(),
            Some("This provider is still starting on the Host")
        );
    }

    #[test]
    fn display_reasons_are_bounded_to_256_utf8_bytes() {
        let availability = FeatureAvailability::unknown(&"🦀".repeat(100));
        let reason = availability.reason.expect("reason");
        assert!(reason.len() <= MAX_REASON_BYTES);
        assert!(reason.is_char_boundary(reason.len()));
    }

    #[test]
    fn sessions_page_clamps_rows_and_emits_bounded_cursor() {
        let store = AppStoreReducer::new();
        store.upsert_server(
            &ServerConfig {
                server_id: "server".to_string(),
                display_name: "H".repeat(1_000),
                host: "localhost".to_string(),
                port: 0,
                websocket_url: None,
                is_local: false,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );
        let threads = (0..150).map(thread_info).collect::<Vec<_>>();
        store.upsert_thread_list_page("server", &threads);

        let page = project_sessions_page(&store.snapshot(), None, Some(u32::MAX)).expect("page");
        assert_eq!(page.rows.len(), MAX_SESSION_PAGE_ROWS);
        assert_eq!(page.total_count, 150);
        assert_eq!(page.next_cursor.as_deref(), Some("v1:100"));
        assert!(page.rows.iter().all(|row| row.host_label.len() <= 256));
        assert!(page.rows.iter().all(|row| row.title.len() <= 256));
        assert!(
            page.rows
                .iter()
                .all(|row| row.preview.as_ref().unwrap().len() <= 512)
        );
        assert!(
            page.rows
                .iter()
                .all(|row| row.model_label.as_ref().unwrap().len() <= 128)
        );
        assert!(serde_json::to_vec(&page).expect("serialize").len() <= 256 * 1024);
    }

    #[test]
    fn invalid_sessions_cursor_fails_closed() {
        let error = project_sessions_page(&AppSnapshot::default(), Some("v1:not-a-number"), None)
            .expect_err("invalid cursor");
        assert_eq!(error, "invalid Sessions cursor");
    }

    #[test]
    fn empty_mission_control_is_bounded_and_stable() {
        let projection = project_mission_control(&AppSnapshot::default());
        assert!(projection.needs_you.is_empty());
        assert!(projection.active.is_empty());
        assert!(projection.recent.is_empty());
        let bytes = serde_json::to_vec(&projection).expect("serialize");
        assert!(bytes.len() <= 512 * 1024);
        assert_eq!(
            bytes,
            serde_json::to_vec(&projection).expect("serialize twice")
        );
    }
}
