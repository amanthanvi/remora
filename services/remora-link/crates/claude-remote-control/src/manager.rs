use serde_json::{Value, json};
use uuid::Uuid;

use crate::api::BridgeApiClient;
use crate::error::RemoteControlError;
use crate::wire::{
    ControlCancelRequestEvent, ControlRequest, PermissionBehavior, PermissionMode,
    PermissionResponseEvent, RemoteEvent, SubscriptionTarget,
};

#[derive(Clone)]
pub struct RemoteSessionManager {
    client: BridgeApiClient,
    session_id: String,
}

#[derive(Clone)]
pub struct RemoteSessionManagerBuilder {
    client: BridgeApiClient,
    session_id: String,
}

impl RemoteSessionManagerBuilder {
    pub fn new(client: BridgeApiClient, session_id: impl Into<String>) -> Self {
        Self {
            client,
            session_id: session_id.into(),
        }
    }

    pub fn build(self) -> RemoteSessionManager {
        RemoteSessionManager {
            client: self.client,
            session_id: self.session_id,
        }
    }
}

impl RemoteSessionManager {
    pub fn builder(
        client: BridgeApiClient,
        session_id: impl Into<String>,
    ) -> RemoteSessionManagerBuilder {
        RemoteSessionManagerBuilder::new(client, session_id)
    }

    pub fn new(client: BridgeApiClient, session_id: impl Into<String>) -> Self {
        Self::builder(client, session_id).build()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn client(&self) -> &BridgeApiClient {
        &self.client
    }

    pub async fn send_event(&self, event: RemoteEvent) -> Result<(), RemoteControlError> {
        self.client
            .post_session_events(&self.session_id, vec![event])
            .await
    }

    pub async fn send_raw_event(&self, event: Value) -> Result<(), RemoteControlError> {
        self.client
            .post_session_events(&self.session_id, vec![RemoteEvent::Unknown(event)])
            .await
    }

    pub async fn send_control_request(
        &self,
        request: ControlRequest,
    ) -> Result<String, RemoteControlError> {
        let request_id = new_request_id();
        self.send_event(RemoteEvent::control_request(&request_id, request))
            .await?;
        Ok(request_id)
    }

    pub async fn send_control_request_value(
        &self,
        request: Value,
    ) -> Result<String, RemoteControlError> {
        let request_id = new_request_id();
        self.send_raw_event(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        }))
        .await?;
        Ok(request_id)
    }

    pub async fn send_control_cancel_request(
        &self,
        request_id: impl Into<String>,
    ) -> Result<(), RemoteControlError> {
        self.send_event(RemoteEvent::Known(
            crate::wire::KnownRemoteEvent::ControlCancelRequest(ControlCancelRequestEvent {
                request_id: request_id.into(),
            }),
        ))
        .await
    }

    pub async fn send_message(&self, text: impl Into<String>) -> Result<(), RemoteControlError> {
        self.send_event(RemoteEvent::user_text(text)).await
    }

    pub async fn send_bash_command(
        &self,
        command: impl Into<String>,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request_value(json!({
            "subtype": "send_bash",
            "command": command.into(),
        }))
        .await
    }

    pub async fn cancel_session(&self) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::Interrupt).await
    }

    pub async fn set_permission_mode(
        &self,
        mode: impl Into<PermissionMode>,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::SetPermissionMode { mode: mode.into() })
            .await
    }

    pub async fn set_model(&self, model: impl Into<String>) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::SetModel {
            model: model.into(),
        })
        .await
    }

    pub async fn respond_to_permission_request(
        &self,
        tool_use_id: impl Into<String>,
        behavior: PermissionBehavior,
        updated_input: Option<Value>,
        message: Option<String>,
    ) -> Result<(), RemoteControlError> {
        self.send_event(RemoteEvent::permission_response(PermissionResponseEvent {
            tool_use_id: tool_use_id.into(),
            behavior,
            updated_input,
            permission_updates: None,
            message,
            extra: Default::default(),
        }))
        .await
    }

    pub async fn subscribe_pr(
        &self,
        repo: impl Into<String>,
        pr_number: u64,
        task_id: Option<String>,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::Subscribe {
            target: SubscriptionTarget::PullRequest {
                repo: repo.into(),
                pr_number,
                task_id,
            },
        })
        .await
    }

    pub async fn unsubscribe_pr(
        &self,
        repo: impl Into<String>,
        pr_number: u64,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::Unsubscribe {
            target: SubscriptionTarget::PullRequest {
                repo: repo.into(),
                pr_number,
                task_id: None,
            },
        })
        .await
    }

    pub async fn subscribe_slack_thread(
        &self,
        channel_id: impl Into<String>,
        ts: impl Into<String>,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::Subscribe {
            target: SubscriptionTarget::SlackThread {
                channel_id: channel_id.into(),
                ts: ts.into(),
            },
        })
        .await
    }

    pub async fn unsubscribe_slack_thread(
        &self,
        channel_id: impl Into<String>,
        ts: impl Into<String>,
    ) -> Result<String, RemoteControlError> {
        self.send_control_request(ControlRequest::Unsubscribe {
            target: SubscriptionTarget::SlackThread {
                channel_id: channel_id.into(),
                ts: ts.into(),
            },
        })
        .await
    }
}

fn new_request_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ControlRequestEnvelope;

    #[test]
    fn raw_bash_control_request_shape_matches_remote_control_rpc() {
        let request_id = "req";
        let event = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "send_bash",
                "command": "cargo test"
            }
        });
        assert_eq!(event["request"]["subtype"], "send_bash");
    }

    #[test]
    fn typed_set_model_control_request_round_trips() {
        let event = RemoteEvent::control_request(
            "req_1",
            ControlRequest::SetModel {
                model: "sonnet".to_string(),
            },
        );
        let value = event.into_value().unwrap();
        let parsed: RemoteEvent = serde_json::from_value(value).unwrap();
        match parsed {
            RemoteEvent::Known(crate::wire::KnownRemoteEvent::ControlRequest(
                ControlRequestEnvelope { request, .. },
            )) => assert_eq!(
                request,
                ControlRequest::SetModel {
                    model: "sonnet".to_string()
                }
            ),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
