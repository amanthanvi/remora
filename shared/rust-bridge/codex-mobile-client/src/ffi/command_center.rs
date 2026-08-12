//! Handwritten, bounded UniFFI contract for command-center capability state.

use crate::store::boundary::session_summaries_from_snapshot;
use crate::store::{AppSessionSummary, AppSnapshot};
use crate::types::{ThreadKey, ThreadSummaryStatus};

const MAX_REASON_BYTES: usize = 256;
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
    pub availability: FeatureAvailability,
    pub capabilities: HostCapabilitiesV1,
    pub provider_instances: Vec<ProviderInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandCenterStatusV1 {
    pub schema_version: u32,
    pub hosts: Vec<HostCommandCenterStatus>,
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

pub(crate) fn project_command_center_status(snapshot: &AppSnapshot) -> CommandCenterStatusV1 {
    let mut hosts = snapshot
        .servers
        .values()
        .map(|server| {
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
    hosts.sort_by(|left, right| left.legacy_server_id.cmp(&right.legacy_server_id));
    CommandCenterStatusV1 {
        schema_version: 1,
        hosts,
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

        let projection = project_command_center_status(&store.snapshot());
        assert_eq!(projection.hosts.len(), 1);
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

        let projection = project_command_center_status(&store.snapshot());
        assert_eq!(
            projection.hosts[0].availability.state,
            AvailabilityState::Unavailable
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
