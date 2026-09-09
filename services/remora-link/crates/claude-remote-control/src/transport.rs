use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::error::RemoteControlError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    WebSocket,
    CcrV2SsePost,
    WebSocketPostOut,
}

pub fn choose_transport_for_url(
    url: &Url,
    force_ccr_v2: bool,
    post_for_session_ingress_v2: bool,
) -> Result<TransportKind, RemoteControlError> {
    if force_ccr_v2 {
        return Ok(TransportKind::CcrV2SsePost);
    }
    match url.scheme() {
        "ws" | "wss" if post_for_session_ingress_v2 => Ok(TransportKind::WebSocketPostOut),
        "ws" | "wss" => Ok(TransportKind::WebSocket),
        other => Err(RemoteControlError::Protocol(format!(
            "Unsupported Remote Control transport URL scheme `{other}`"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub retry: Option<u64>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
    event: Option<String>,
    id: Option<String>,
    retry: Option<u64>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_str(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buffer.push_str(chunk);
        let mut frames = Vec::new();
        while let Some(idx) = self.buffer.find("\n\n") {
            let raw = self.buffer[..idx].to_string();
            self.buffer = self.buffer[idx + 2..].to_string();
            if let Some(frame) = self.parse_frame(&raw) {
                frames.push(frame);
            }
        }
        frames
    }

    pub fn finish(&mut self) -> Option<SseFrame> {
        if self.buffer.trim().is_empty() {
            return None;
        }
        let raw = std::mem::take(&mut self.buffer);
        self.parse_frame(&raw)
    }

    fn parse_frame(&mut self, raw: &str) -> Option<SseFrame> {
        self.event = None;
        self.id = None;
        self.retry = None;
        self.data.clear();
        for line in raw.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "id" => self.id = Some(value.to_string()),
                "retry" => self.retry = value.parse().ok(),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        if self.event.is_none() && self.id.is_none() && self.retry.is_none() && self.data.is_empty()
        {
            return None;
        }
        Some(SseFrame {
            event: self.event.take(),
            id: self.id.take(),
            retry: self.retry.take(),
            data: self.data.join("\n"),
        })
    }
}

pub struct WebSocketJsonTransport {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WebSocketJsonTransport {
    pub async fn connect(url: Url) -> Result<Self, RemoteControlError> {
        let (inner, _) = connect_async(url.as_str()).await?;
        Ok(Self { inner })
    }

    pub async fn send_json<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), RemoteControlError> {
        let text = serde_json::to_string(value)?;
        self.inner.send(Message::Text(text.into())).await?;
        Ok(())
    }

    pub async fn recv_json<T: DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, RemoteControlError> {
        while let Some(message) = self.inner.next().await {
            match message? {
                Message::Text(text) => return Ok(Some(serde_json::from_str(&text)?)),
                Message::Binary(bytes) => return Ok(Some(serde_json::from_slice(&bytes)?)),
                Message::Ping(payload) => self.inner.send(Message::Pong(payload)).await?,
                Message::Pong(_) => {}
                Message::Close(_) => return Ok(None),
                Message::Frame(_) => {}
            }
        }
        Ok(None)
    }

    pub async fn close(mut self) -> Result<(), RemoteControlError> {
        self.inner.close(None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_transport_like_claude() {
        let url = Url::parse("wss://api.example/v1/session_ingress/ws/sid").unwrap();
        assert_eq!(
            choose_transport_for_url(&url, false, false).unwrap(),
            TransportKind::WebSocket
        );
        assert_eq!(
            choose_transport_for_url(&url, true, false).unwrap(),
            TransportKind::CcrV2SsePost
        );
        assert_eq!(
            choose_transport_for_url(&url, false, true).unwrap(),
            TransportKind::WebSocketPostOut
        );
    }

    #[test]
    fn sse_decoder_handles_multiline_data_and_ids() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push_str("id: 1\nevent: message\ndata: {\"a\":1}\ndata: tail\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                event: Some("message".to_string()),
                id: Some("1".to_string()),
                retry: None,
                data: "{\"a\":1}\ntail".to_string(),
            }]
        );
    }
}
