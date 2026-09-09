use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::RemoteControlError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionMode(pub String);

impl PermissionMode {
    pub const DEFAULT: &'static str = "default";
    pub const ACCEPT_EDITS: &'static str = "acceptEdits";
    pub const BYPASS_PERMISSIONS: &'static str = "bypassPermissions";
    pub const PLAN: &'static str = "plan";

    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self(Self::DEFAULT.to_string())
    }
}

impl From<&str> for PermissionMode {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeEnvironmentRegistration {
    pub machine_name: String,
    pub directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_url: Option<String>,
    pub max_sessions: usize,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

impl BridgeEnvironmentRegistration {
    pub fn new(machine_name: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("worker_type".to_string(), json!("remote-control"));
        Self {
            machine_name: machine_name.into(),
            directory: directory.into(),
            branch: None,
            git_repo_url: None,
            max_sessions: 1,
            metadata,
            environment_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeEnvironmentRegistrationResponse {
    pub environment_id: String,
    pub environment_secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    pub secret: String,
    pub data: WorkData,
}

impl Work {
    pub fn decode_secret(&self) -> Result<WorkSecret, RemoteControlError> {
        WorkSecret::decode(&self.secret)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkData {
    Healthcheck {
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    Session {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_ingress_url: Option<String>,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSecret {
    pub version: u32,
    pub session_ingress_token: String,
    pub api_base_url: String,
    #[serde(default)]
    pub use_code_sessions: bool,
}

impl WorkSecret {
    pub fn decode(encoded: &str) -> Result<Self, RemoteControlError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(encoded))
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(encoded))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn encode_url_safe_no_pad(&self) -> Result<String, RemoteControlError> {
        let bytes = serde_json::to_vec(self)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkStopRequest {
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkHeartbeatResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_extended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconnectSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReconnectSessionResponse {
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    #[serde(default)]
    pub events: Vec<RemoteEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context: Option<SessionContext>,
    pub environment_id: String,
    pub source: SessionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionSource {
    RemoteControl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ingress_url: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionsPage {
    #[serde(default, alias = "sessions")]
    pub data: Vec<SessionRecord>,
    #[serde(
        default,
        alias = "next_cursor",
        alias = "nextCursor",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_page: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventsPostRequest {
    pub events: Vec<RemoteEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventsPage {
    #[serde(default, alias = "events")]
    pub data: Vec<RemoteEvent>,
    #[serde(
        default,
        alias = "next_cursor",
        alias = "nextCursor",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_page: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RemoteEvent {
    Known(KnownRemoteEvent),
    Unknown(Value),
}

impl RemoteEvent {
    pub fn into_value(self) -> Result<Value, RemoteControlError> {
        Ok(serde_json::to_value(self)?)
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::Known(KnownRemoteEvent::User(TranscriptEvent {
            role: TranscriptRole::User,
            content: TranscriptContent::Text(text.into()),
            historical: false,
            extra: BTreeMap::new(),
        }))
    }

    pub fn control_request(request_id: impl Into<String>, request: ControlRequest) -> Self {
        Self::Known(KnownRemoteEvent::ControlRequest(ControlRequestEnvelope {
            request_id: request_id.into(),
            request,
            extra: BTreeMap::new(),
        }))
    }

    pub fn permission_response(event: PermissionResponseEvent) -> Self {
        Self::Known(KnownRemoteEvent::PermissionResponse(event))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownRemoteEvent {
    User(TranscriptEvent),
    Assistant(TranscriptEvent),
    System(TranscriptEvent),
    ControlRequest(ControlRequestEnvelope),
    ControlResponse(ControlResponseEnvelope),
    ControlCancelRequest(ControlCancelRequestEvent),
    PermissionRequest(PermissionRequestEvent),
    PermissionResponse(PermissionResponseEvent),
    SandboxPermissionRequest(SandboxPermissionRequestEvent),
    SandboxPermissionResponse(SandboxPermissionResponseEvent),
    Result(ResultEvent),
    StreamEvent(OpaqueEvent),
    TranscriptMirror(OpaqueEvent),
    KeepAlive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub role: TranscriptRole,
    pub content: TranscriptContent,
    #[serde(default, skip_serializing_if = "is_false")]
    pub historical: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TranscriptContent {
    Text(String),
    Blocks(Vec<Value>),
    Object(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpaqueEvent {
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRequestEnvelope {
    pub request_id: String,
    pub request: ControlRequest,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequest {
    Initialize {
        #[serde(default, flatten)]
        fields: BTreeMap<String, Value>,
    },
    Interrupt,
    CanUseTool {
        tool_name: String,
        input: Value,
        #[serde(default, flatten)]
        extra: BTreeMap<String, Value>,
    },
    SetPermissionMode {
        mode: PermissionMode,
    },
    SetModel {
        model: String,
    },
    SetMaxThinkingTokens {
        tokens: u32,
    },
    RenameSession {
        name: String,
    },
    SetColor {
        color: String,
    },
    FileSuggestions {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    ReadFile {
        path: PathBuf,
    },
    McpStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
    },
    McpAuthenticate {
        server_name: String,
    },
    McpOauthCallbackUrl {
        server_name: String,
        url: String,
    },
    McpReconnect {
        server_name: String,
    },
    RemoteControl {
        enable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    PermissionResponse {
        tool_use_id: String,
        behavior: PermissionBehavior,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    PermissionUpdates {
        updates: Value,
    },
    MessageRated {
        message_id: String,
        rating: String,
    },
    RewindFiles {
        user_message_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dry_run: Option<bool>,
    },
    CancelAsyncMessage {
        message_id: String,
    },
    SeedReadState {
        state: Value,
    },
    SubmitFeedback {
        feedback: Value,
    },
    GenerateSessionTitle,
    AskSideQuestion {
        prompt: String,
    },
    Subscribe {
        target: SubscriptionTarget,
    },
    Unsubscribe {
        target: SubscriptionTarget,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriptionTarget {
    PullRequest {
        repo: String,
        pr_number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    SlackThread {
        channel_id: String,
        ts: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlResponseEnvelope {
    pub response: ControlResponse,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResponse {
    Success {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
    },
    Error {
        request_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlCancelRequestEvent {
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequestEvent {
    pub tool_use_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_suggestions: Vec<PermissionSuggestion>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionSuggestion {
    pub behavior: PermissionBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionResponseEvent {
    pub tool_use_id: String,
    pub behavior: PermissionBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_updates: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxPermissionRequestEvent {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default)]
    pub input: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxPermissionResponseEvent {
    pub request_id: String,
    pub behavior: PermissionBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRegisterResponse {
    pub worker_epoch: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerStateResponse {
    #[serde(flatten)]
    pub state: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerInitRequest {
    pub worker_epoch: String,
    #[serde(default, flatten)]
    pub state: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHeartbeatRequest {
    pub session_id: String,
    pub worker_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEventsRequest {
    pub worker_epoch: String,
    pub events: Vec<RemoteEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEventsPage {
    #[serde(default)]
    pub data: Vec<RemoteEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerDeliveryAckRequest {
    pub worker_epoch: String,
    pub updates: Vec<WorkerDeliveryUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerDeliveryUpdate {
    pub event_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroveAccountSettings {
    #[serde(flatten)]
    pub settings: BTreeMap<String, Value>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn work_secret_decodes_url_safe_base64_json() {
        let secret = WorkSecret {
            version: 1,
            session_ingress_token: "tok".to_string(),
            api_base_url: "https://api.anthropic.com".to_string(),
            use_code_sessions: true,
        };
        let encoded = secret.encode_url_safe_no_pad().unwrap();
        assert_eq!(WorkSecret::decode(&encoded).unwrap(), secret);
    }

    #[test]
    fn control_request_serializes_subtype_shape() {
        let event = RemoteEvent::control_request(
            "req_1",
            ControlRequest::SetPermissionMode {
                mode: PermissionMode::new(PermissionMode::ACCEPT_EDITS),
            },
        );
        let value = event.into_value().unwrap();
        assert_eq!(
            value,
            json!({
                "type": "control_request",
                "request_id": "req_1",
                "request": {
                    "subtype": "set_permission_mode",
                    "mode": "acceptEdits"
                }
            })
        );
    }

    #[test]
    fn parses_permission_request() {
        let event: RemoteEvent = serde_json::from_value(json!({
            "type": "permission_request",
            "tool_use_id": "toolu_1",
            "tool_name": "Bash",
            "description": "run tests",
            "input": {"command": "cargo test"},
            "permission_suggestions": [{"behavior": "allow"}]
        }))
        .unwrap();
        match event {
            RemoteEvent::Known(KnownRemoteEvent::PermissionRequest(req)) => {
                assert_eq!(req.tool_name, "Bash");
                assert_eq!(
                    req.permission_suggestions[0].behavior,
                    PermissionBehavior::Allow
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn sessions_page_accepts_data_or_sessions_key() {
        let by_data: SessionsPage = serde_json::from_value(json!({
            "data": [{"id": "sess_1", "title": "one"}],
            "next_page": "next"
        }))
        .unwrap();
        assert_eq!(by_data.data[0].id, "sess_1");
        assert_eq!(by_data.next_page.as_deref(), Some("next"));

        let by_sessions: SessionsPage = serde_json::from_value(json!({
            "sessions": [{"id": "sess_2"}],
            "next_cursor": "old"
        }))
        .unwrap();
        assert_eq!(by_sessions.data[0].id, "sess_2");
        assert_eq!(by_sessions.next_page.as_deref(), Some("old"));
    }

    #[test]
    fn event_pages_parse_known_and_unknown_events() {
        let page: SessionEventsPage = serde_json::from_value(json!({
            "data": [
                {"type": "user", "role": "user", "content": "hi"},
                {"type": "future_event", "field": true}
            ],
            "next_page": "p2"
        }))
        .unwrap();
        assert_eq!(page.data.len(), 2);
        assert!(matches!(
            page.data[0],
            RemoteEvent::Known(KnownRemoteEvent::User(_))
        ));
        assert!(matches!(page.data[1], RemoteEvent::Unknown(_)));

        let legacy: SessionEventsPage = serde_json::from_value(json!({
            "events": [{"type": "future_event"}],
            "next_cursor": "c2"
        }))
        .unwrap();
        assert_eq!(legacy.data.len(), 1);
        assert_eq!(legacy.next_page.as_deref(), Some("c2"));
    }
}
