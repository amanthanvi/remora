use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode,
    header::{AUTHORIZATION, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::{ports::*, types::*};

/// Concrete relay HTTP adapter shared by iOS and Android.
///
/// Redirects are disabled at the client and checked again at the response
/// seam. Response bodies are streamed into a bounded buffer. Authorization
/// values are marked sensitive and no request or response is logged.
pub(crate) struct ReqwestRelayTransport {
    client: Client,
}

impl ReqwestRelayTransport {
    pub(crate) fn new() -> Result<Self, RelayError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| RelayError::PermanentFailure)?;
        Ok(Self { client })
    }

    fn request(
        &self,
        context: &RelayTransportContext,
        method: Method,
        path: &str,
    ) -> Result<RequestBuilder, RelayTransportError> {
        if context.operation.follow_redirects {
            return Err(RelayTransportError::RedirectRejected);
        }
        let url = context
            .origin
            .as_url()
            .join(path)
            .map_err(|_| RelayTransportError::InvalidResponse)?;
        let mut authorization = Zeroizing::new(Vec::with_capacity(
            7 + context.authorization.expose_for_adapter().len(),
        ));
        authorization.extend_from_slice(b"Bearer ");
        authorization.extend_from_slice(context.authorization.expose_for_adapter());
        let mut header = HeaderValue::from_bytes(&authorization)
            .map_err(|_| RelayTransportError::InvalidResponse)?;
        header.set_sensitive(true);
        Ok(self
            .client
            .request(method, url)
            .header(AUTHORIZATION, header))
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        max_response_bytes: usize,
    ) -> Result<(T, usize), RelayTransportError> {
        let response = request.send().await.map_err(map_reqwest_error)?;
        let response = ensure_success(response)?;
        let bytes = read_bounded(response, max_response_bytes).await?;
        let encoded_bytes = bytes.len();
        let value =
            serde_json::from_slice(&bytes).map_err(|_| RelayTransportError::InvalidResponse)?;
        Ok((value, encoded_bytes))
    }

    async fn send_empty(&self, request: RequestBuilder) -> Result<(), RelayTransportError> {
        let response = request.send().await.map_err(map_reqwest_error)?;
        ensure_success(response)?;
        Ok(())
    }
}

#[async_trait]
impl RelayTransportPort for ReqwestRelayTransport {
    async fn register_device(
        &self,
        context: RelayTransportContext,
        request: RelayRegisterDeviceRequest,
    ) -> Result<RelayDeviceRegistrationReceipt, RelayTransportError> {
        let installation = &request.installation_id.0;
        let path = format!("v1/installations/{installation}/devices");
        let token = std::str::from_utf8(request.token.expose_for_adapter())
            .map_err(|_| RelayTransportError::InvalidResponse)?;
        let payload = RegisterDeviceWireRequest {
            provider: request.provider.into(),
            environment: request.environment.into(),
            token,
        };
        let builder = self.request(&context, Method::POST, &path)?.json(&payload);
        let (receipt, _) = self
            .send_json::<RegisterDeviceWireResponse>(builder, context.operation.max_response_bytes)
            .await?;
        receipt.try_into()
    }

    async fn tombstone_device(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneDeviceRequest,
    ) -> Result<(), RelayTransportError> {
        let installation = &request.installation_id.0;
        let registration = &request.registration_id.0;
        let path = format!("v1/installations/{installation}/devices/{registration}");
        let builder = self
            .request(&context, Method::DELETE, &path)?
            .query(&[("through_generation", request.through_generation)]);
        self.send_empty(builder).await
    }

    async fn fetch_events(
        &self,
        context: RelayTransportContext,
        request: RelayFetchEventsRequest,
    ) -> Result<RelayEventPage, RelayTransportError> {
        let installation = &request.installation_id.0;
        let path = format!("v1/installations/{installation}/events");
        let builder = self.request(&context, Method::GET, &path)?.query(&[
            ("after", request.after),
            ("limit", u64::from(request.limit)),
        ]);
        let (page, encoded_bytes) = self
            .send_json::<EventPageWire>(builder, context.operation.max_response_bytes)
            .await?;
        page.into_relay(encoded_bytes)
    }

    async fn fetch_snapshot(
        &self,
        context: RelayTransportContext,
        request: RelayFetchSnapshotRequest,
    ) -> Result<RelaySnapshotEnvelope, RelayTransportError> {
        let installation = &request.installation_id.0;
        let path = format!("v1/installations/{installation}/snapshot");
        let builder = self.request(&context, Method::GET, &path)?;
        let (snapshot, encoded_bytes) = self
            .send_json::<SnapshotWire>(builder, context.operation.max_response_bytes)
            .await?;
        snapshot.into_relay(encoded_bytes)
    }

    async fn acknowledge(
        &self,
        context: RelayTransportContext,
        request: RelayAckRequest,
    ) -> Result<RelayAckReceipt, RelayTransportError> {
        let installation = &request.installation_id.0;
        let path = format!("v1/installations/{installation}/ack");
        let payload = AcknowledgeWireRequest {
            schema_version: RELAY_SCHEMA_VERSION,
            through_cursor: request.through_cursor,
        };
        let builder = self.request(&context, Method::PUT, &path)?.json(&payload);
        let (receipt, _) = self
            .send_json::<AcknowledgeWireResponse>(builder, context.operation.max_response_bytes)
            .await?;
        receipt.try_into()
    }

    async fn tombstone_installation(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneInstallationRequest,
    ) -> Result<(), RelayTransportError> {
        let installation = &request.installation_id.0;
        let path = format!("v1/installations/{installation}");
        let builder = self.request(&context, Method::DELETE, &path)?;
        self.send_empty(builder).await
    }
}

fn ensure_success(response: Response) -> Result<Response, RelayTransportError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(map_status(status))
}

fn map_status(status: StatusCode) -> RelayTransportError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => RelayTransportError::Unauthorized,
        StatusCode::NOT_FOUND => RelayTransportError::NotFound,
        StatusCode::GONE => RelayTransportError::Gone,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => RelayTransportError::Timeout,
        StatusCode::TOO_MANY_REQUESTS => RelayTransportError::RateLimited,
        StatusCode::PAYLOAD_TOO_LARGE => RelayTransportError::ResponseTooLarge,
        status if status.is_redirection() => RelayTransportError::RedirectRejected,
        status if status.is_server_error() => RelayTransportError::Server,
        _ => RelayTransportError::InvalidResponse,
    }
}

fn map_reqwest_error(error: reqwest::Error) -> RelayTransportError {
    if error.is_timeout() {
        RelayTransportError::Timeout
    } else if error.is_redirect() {
        RelayTransportError::RedirectRejected
    } else if error.is_body() || error.is_decode() {
        RelayTransportError::InvalidResponse
    } else {
        RelayTransportError::Network
    }
}

async fn read_bounded(
    response: Response,
    max_response_bytes: usize,
) -> Result<Vec<u8>, RelayTransportError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(RelayTransportError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(RelayTransportError::ResponseTooLarge)?;
        if next_len > max_response_bytes {
            return Err(RelayTransportError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Serialize)]
struct RegisterDeviceWireRequest<'a> {
    provider: PushProviderWire,
    environment: PushEnvironmentWire,
    token: &'a str,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PushProviderWire {
    Apns,
    Fcm,
}

impl From<RelayPushProvider> for PushProviderWire {
    fn from(value: RelayPushProvider) -> Self {
        match value {
            RelayPushProvider::Apns => Self::Apns,
            RelayPushProvider::Fcm => Self::Fcm,
        }
    }
}

impl From<PushProviderWire> for RelayPushProvider {
    fn from(value: PushProviderWire) -> Self {
        match value {
            PushProviderWire::Apns => Self::Apns,
            PushProviderWire::Fcm => Self::Fcm,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PushEnvironmentWire {
    Sandbox,
    Production,
}

impl From<RelayPushEnvironment> for PushEnvironmentWire {
    fn from(value: RelayPushEnvironment) -> Self {
        match value {
            RelayPushEnvironment::Sandbox => Self::Sandbox,
            RelayPushEnvironment::Production => Self::Production,
        }
    }
}

impl From<PushEnvironmentWire> for RelayPushEnvironment {
    fn from(value: PushEnvironmentWire) -> Self {
        match value {
            PushEnvironmentWire::Sandbox => Self::Sandbox,
            PushEnvironmentWire::Production => Self::Production,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterDeviceWireResponse {
    schema_version: u16,
    installation_id: String,
    registration_id: String,
    provider: PushProviderWire,
    environment: PushEnvironmentWire,
    generation: u64,
    replaced: bool,
}

impl TryFrom<RegisterDeviceWireResponse> for RelayDeviceRegistrationReceipt {
    type Error = RelayTransportError;

    fn try_from(value: RegisterDeviceWireResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema_version: value.schema_version,
            installation_id: RelayInstallationId::parse(value.installation_id)
                .map_err(|_| RelayTransportError::InvalidResponse)?,
            registration_id: RelayRegistrationId::parse(value.registration_id)
                .map_err(|_| RelayTransportError::InvalidResponse)?,
            provider: value.provider.into(),
            environment: value.environment.into(),
            generation: value.generation,
            replaced: value.replaced,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // Exact relay wire vocabulary.
enum EventClassWire {
    StateChanged,
    ActivityChanged,
    ConnectionChanged,
    SecurityChanged,
}

impl From<EventClassWire> for RelayEventClass {
    fn from(value: EventClassWire) -> Self {
        match value {
            EventClassWire::StateChanged => Self::StateChanged,
            EventClassWire::ActivityChanged => Self::ActivityChanged,
            EventClassWire::ConnectionChanged => Self::ConnectionChanged,
            EventClassWire::SecurityChanged => Self::SecurityChanged,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    event_id: String,
    cursor: u64,
    event_class: EventClassWire,
    expires_at_ms: i64,
    #[serde(rename = "ciphertext")]
    _ciphertext: serde::de::IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventPageWire {
    schema_version: u16,
    requested_after: u64,
    next_cursor: u64,
    high_watermark: u64,
    replay_floor: u64,
    reset_required: bool,
    snapshot_available: bool,
    events: Vec<EventWire>,
}

impl EventPageWire {
    fn into_relay(self, encoded_bytes: usize) -> Result<RelayEventPage, RelayTransportError> {
        let events = self
            .events
            .into_iter()
            .map(|event| {
                let EventWire {
                    event_id,
                    cursor,
                    event_class,
                    expires_at_ms,
                    _ciphertext: _,
                } = event;
                Ok(RelayEventEnvelope {
                    event_id: RelayEventId::parse(event_id)
                        .map_err(|_| RelayTransportError::InvalidResponse)?,
                    cursor,
                    event_class: event_class.into(),
                    expires_at_ms: u64::try_from(expires_at_ms)
                        .map_err(|_| RelayTransportError::InvalidResponse)?,
                })
            })
            .collect::<Result<Vec<_>, RelayTransportError>>()?;
        Ok(RelayEventPage {
            schema_version: self.schema_version,
            requested_after: self.requested_after,
            next_cursor: self.next_cursor,
            high_watermark: self.high_watermark,
            replay_floor: self.replay_floor,
            reset_required: self.reset_required,
            snapshot_available: self.snapshot_available,
            events,
            encoded_bytes,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    schema_version: u16,
    revision: u64,
    through_cursor: u64,
    expires_at_ms: i64,
    #[serde(rename = "ciphertext")]
    _ciphertext: serde::de::IgnoredAny,
}

impl SnapshotWire {
    fn into_relay(
        self,
        encoded_bytes: usize,
    ) -> Result<RelaySnapshotEnvelope, RelayTransportError> {
        let Self {
            schema_version,
            revision,
            through_cursor,
            expires_at_ms,
            _ciphertext: _,
        } = self;
        Ok(RelaySnapshotEnvelope {
            schema_version,
            revision,
            through_cursor,
            expires_at_ms: u64::try_from(expires_at_ms)
                .map_err(|_| RelayTransportError::InvalidResponse)?,
            encoded_bytes,
        })
    }
}

#[derive(Serialize)]
struct AcknowledgeWireRequest {
    schema_version: u16,
    through_cursor: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeWireResponse {
    schema_version: u16,
    installation_id: String,
    acknowledged_through: u64,
    replayed: bool,
}

impl TryFrom<AcknowledgeWireResponse> for RelayAckReceipt {
    type Error = RelayTransportError;

    fn try_from(value: AcknowledgeWireResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            schema_version: value.schema_version,
            installation_id: RelayInstallationId::parse(value.installation_id)
                .map_err(|_| RelayTransportError::InvalidResponse)?,
            acknowledged_through: value.acknowledged_through,
            replayed: value.replayed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_service_event_fixture_discards_ciphertext() {
        let fixture = br#"{
            "schema_version":1,
            "requested_after":0,
            "next_cursor":1,
            "high_watermark":1,
            "replay_floor":0,
            "reset_required":false,
            "snapshot_available":false,
            "events":[{
                "event_id":"event_identifier_0001",
                "cursor":1,
                "event_class":"security_changed",
                "expires_at_ms":9999999999999,
                "ciphertext":"secret-ciphertext-sentinel"
            }]
        }"#;
        let wire: EventPageWire = serde_json::from_slice(fixture).expect("valid fixture");
        let page = wire.into_relay(fixture.len()).expect("valid relay page");
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event_class, RelayEventClass::SecurityChanged);
        let debug = format!("{page:?}");
        assert!(!debug.contains("secret-ciphertext-sentinel"));
    }

    #[test]
    fn status_mapping_is_closed_and_fail_closed() {
        assert_eq!(
            map_status(StatusCode::UNAUTHORIZED),
            RelayTransportError::Unauthorized
        );
        assert_eq!(
            map_status(StatusCode::PERMANENT_REDIRECT),
            RelayTransportError::RedirectRejected
        );
        assert_eq!(
            map_status(StatusCode::PAYLOAD_TOO_LARGE),
            RelayTransportError::ResponseTooLarge
        );
        assert_eq!(
            map_status(StatusCode::IM_A_TEAPOT),
            RelayTransportError::InvalidResponse
        );
    }
}
