//! Retain the Codex transport independently of the phone attachment. Only
//! notifications/requests replay; responses stay bound to their original caller.

use anyhow::{anyhow, ensure};
use futures::{SinkExt, StreamExt};
use remora_bridge_core::session::{Session, SessionRegistry};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream, ReadHalf,
        WriteHalf,
    },
    sync::{Mutex, mpsc},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::agents::StartedAgentSession;

const MAX_FRAME: usize = 4 * 1024 * 1024;
const MAX_PENDING: usize = 256;
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}
type BoxIo = Box<dyn Io>;

enum Wire {
    Websocket(Box<WebSocketStream<BoxIo>>),
    Jsonl(BufReader<ReadHalf<BoxIo>>, WriteHalf<BoxIo>, Vec<u8>),
}

impl Wire {
    async fn open<S: Io + 'static>(io: S, websocket: bool, client: bool) -> anyhow::Result<Self> {
        let io: BoxIo = Box::new(io);
        if !websocket {
            let (read, write) = tokio::io::split(io);
            return Ok(Self::Jsonl(BufReader::new(read), write, Vec::new()));
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        let ws = if client {
            tokio_tungstenite::client_async_with_config(
                "ws://codex-app-server-proxy.localhost/rpc",
                io,
                Some(config),
            )
            .await?
            .0
        } else {
            tokio_tungstenite::accept_async_with_config(io, Some(config)).await?
        };
        Ok(Self::Websocket(Box::new(ws)))
    }

    async fn read(&mut self) -> anyhow::Result<Option<Value>> {
        match self {
            Self::Websocket(socket) => loop {
                match socket.next().await.transpose()? {
                    Some(Message::Text(text)) => return Ok(Some(serde_json::from_str(&text)?)),
                    Some(Message::Close(_)) | None => return Ok(None),
                    Some(Message::Ping(_)) | Some(Message::Pong(_)) => continue,
                    _ => return Err(anyhow!("non-text Codex frame")),
                }
            },
            Self::Jsonl(reader, _, bytes) => loop {
                let available = reader.fill_buf().await?;
                if available.is_empty() {
                    ensure!(bytes.is_empty(), "truncated Codex frame");
                    return Ok(None);
                }
                let length = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map(|n| n + 1)
                    .unwrap_or(available.len());
                ensure!(bytes.len() + length <= MAX_FRAME, "Codex frame too large");
                bytes.extend_from_slice(&available[..length]);
                reader.consume(length);
                if bytes.last() == Some(&b'\n') {
                    return Ok(Some(serde_json::from_slice(&std::mem::take(bytes))?));
                }
            },
        }
    }

    async fn send(&mut self, value: &Value) -> anyhow::Result<()> {
        let text = serde_json::to_string(value)?;
        ensure!(text.len() <= MAX_FRAME, "Codex frame too large");
        match self {
            Self::Websocket(socket) => socket.send(Message::Text(text.into())).await?,
            Self::Jsonl(_, writer, _) => {
                writer.write_all(text.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }
}

struct Command {
    generation: u64,
    value: Value,
}
struct Retained {
    session: Arc<Session>,
    sender: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Retained {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct BackendGuard {
    session: Arc<Session>,
    registry: Weak<SessionRegistry>,
}
impl Drop for BackendGuard {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.release_if_current(&self.session);
        }
    }
}

#[derive(Default)]
pub(crate) struct CodexSessions {
    sessions: Mutex<HashMap<String, Retained>>,
}

impl CodexSessions {
    pub async fn shutdown(&self) {
        let entries = std::mem::take(&mut *self.sessions.lock().await);
        for (_, mut entry) in entries {
            entry.task.abort();
            let _ = (&mut entry.task).await;
        }
    }

    pub async fn attach<S, F, Fut>(
        &self,
        stream: S,
        session: Arc<Session>,
        last_seen: Option<u64>,
        websocket: bool,
        registry: Weak<SessionRegistry>,
        start: F,
    ) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>>
    where
        S: Io + 'static,
        F: FnOnce(DuplexStream) -> Fut,
        Fut: Future<Output = anyhow::Result<StartedAgentSession>>,
    {
        let mut pool = self.sessions.lock().await;
        pool.retain(|_, entry| !entry.sender.is_closed());
        let existing = pool
            .get(&session.node_id)
            .filter(|entry| Arc::ptr_eq(&entry.session, &session));
        let sender = if let Some(existing) = existing {
            existing.sender.clone()
        } else {
            pool.remove(&session.node_id);
            ensure!(pool.len() < 128, "Codex retained session capacity reached");
            let (client, backend) = tokio::io::duplex(64 * 1024);
            let process = start(backend).await?;
            let backend =
                tokio::time::timeout(Duration::from_secs(5), Wire::open(client, websocket, true))
                    .await??;
            session.invalidate_state_barrier();
            let (sender, receiver) = mpsc::channel(32);
            let runtime_session = Arc::clone(&session);
            let guard = BackendGuard {
                session: Arc::clone(&session),
                registry: registry.clone(),
            };
            let task = tokio::spawn(async move {
                let _guard = guard;
                let result =
                    serve_backend(backend, receiver, Arc::clone(&runtime_session), registry).await;
                // Process ownership remains here after phone detach. Losing the
                // registry/session or upstream transport terminates the helper.
                drop(process);
                if result.is_err() {
                    tracing::warn!("retained Codex transport closed");
                }
            });
            pool.insert(
                session.node_id.clone(),
                Retained {
                    session: Arc::clone(&session),
                    sender: sender.clone(),
                    task,
                },
            );
            sender
        };
        drop(pool);
        // Websocket upgrade belongs to stream serving, not the v2 setup fence:
        // the peer starts its upgrade only after receiving the connect response.
        Ok(tokio::spawn(async move {
            let wire = tokio::time::timeout(
                Duration::from_secs(10),
                Wire::open(stream, websocket, false),
            )
            .await??;
            serve_attachment(wire, session, last_seen, sender).await
        }))
    }
}

struct PendingRequest {
    original_id: Value,
    generation: u64,
    initialize: bool,
    capabilities: Value,
}

async fn serve_backend(
    mut wire: Wire,
    mut receiver: mpsc::Receiver<Command>,
    session: Arc<Session>,
    registry: Weak<SessionRegistry>,
) -> anyhow::Result<()> {
    let mut requests = HashMap::<String, PendingRequest>::new();
    let mut server_requests = HashMap::<String, Value>::new();
    let mut counter = 0u64;
    let mut initialized: Option<(Value, Value)> = None;
    let mut initialized_notification_sent = false;
    let mut cleanup = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            incoming = wire.read() => {
                let Some(mut value) = incoming? else { break; };
                if value.get("method").is_some() {
                    if let Some(id) = value.get("id") {
                        ensure!(server_requests.len() < MAX_PENDING, "Codex server request capacity reached");
                        server_requests.insert(id.to_string(), value.clone());
                    } else if value["method"] == "serverRequest/resolved" {
                        server_requests.remove(&value["params"]["requestId"].to_string());
                    }
                    session.enqueue(value);
                } else if let Some(id) = value.get("id").and_then(Value::as_str)
                    && let Some(request) = requests.remove(id) {
                        if request.initialize && value.get("result").is_some() {
                            initialized = Some((request.capabilities, value["result"].clone()));
                        }
                        if request.generation == session.current_attachment_generation() {
                            value["id"] = request.original_id;
                            session.enqueue(value);
                        }
                }
            }
            command = receiver.recv() => {
                let Some(Command { generation, mut value }) = command else { break; };
                if generation != session.current_attachment_generation() { continue; }
                let is_initialize = value.get("method").and_then(Value::as_str) == Some("initialize");
                if is_initialize && let Some((capabilities, result)) = &initialized {
                        let response = if *capabilities == value["params"]["capabilities"] {
                            serde_json::json!({"id": value["id"], "result": result})
                        } else {
                            serde_json::json!({"id": value["id"], "error": {"code": -32602, "message": "Retained session capabilities changed; reconnect a fresh session"}})
                        };
                        let compatible = response.get("result").is_some();
                        session.enqueue(response);
                        if compatible {
                            for pending in server_requests.values() { session.enqueue(pending.clone()); }
                        }
                        continue;
                }
                if value.get("method").and_then(Value::as_str) == Some("initialized") && initialized.is_some() {
                    // The first initialized notification must reach the backend;
                    // subsequent attachments need no duplicate initialization.
                    if initialized_notification_sent { continue; }
                    initialized_notification_sent = true;
                }
                if value.get("method").is_some() && value.get("id").is_some() {
                    if requests.len() >= MAX_PENDING {
                        session.enqueue(serde_json::json!({"id": value["id"], "error": {"code": -32000, "message": "Host request capacity reached before dispatch"}}));
                        continue;
                    }
                    counter = counter.checked_add(1).ok_or_else(|| anyhow!("Codex request sequence exhausted"))?;
                    let id = format!("remora-host-{counter}");
                    requests.insert(id.clone(), PendingRequest { original_id: value["id"].clone(), generation,
                        initialize: is_initialize, capabilities: value["params"]["capabilities"].clone() });
                    value["id"] = Value::String(id);
                } else if value.get("method").is_none() && value.get("id").is_some()
                    && server_requests.remove(&value["id"].to_string()).is_none() {
                    continue;
                }
                tokio::time::timeout(Duration::from_secs(8), wire.send(&value)).await??;
            }
            _ = cleanup.tick() => {
                let current = registry.upgrade().and_then(|registry| registry.get(&session.node_id, "codex"));
                if current.is_none_or(|current| !Arc::ptr_eq(&current, &session)) { break; }
            }
        }
    }
    Ok(())
}

struct AttachmentGuard {
    session: Arc<Session>,
    generation: u64,
}
impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        self.session.drop_attachment_generation(self.generation);
    }
}

async fn serve_attachment(
    mut wire: Wire,
    session: Arc<Session>,
    last_seen: Option<u64>,
    sender: mpsc::Sender<Command>,
) -> anyhow::Result<()> {
    let mut attachment = session.install_attachment(last_seen);
    let generation = attachment.generation;
    let _guard = AttachmentGuard {
        session: Arc::clone(&session),
        generation,
    };
    for frame in attachment.backlog {
        if frame.payload.get("method").is_some() && frame.payload.get("id").is_none() {
            tokio::time::timeout(Duration::from_secs(8), wire.send(&frame.payload)).await??;
            session.note_drainer_attempt(frame.seq);
        }
    }
    loop {
        tokio::select! {
            frame = wire.read() => {
                let Some(value) = frame? else { break; };
                tokio::time::timeout(Duration::from_secs(8), sender.send(Command { generation, value })).await?.map_err(|_| anyhow!("Codex session unavailable"))?;
            }
            frame = attachment.live_rx.recv() => {
                let Some(frame) = frame else { break; };
                session.note_drainer_attempt(frame.seq);
                tokio::time::timeout(Duration::from_secs(8), wire.send(&frame.payload)).await??;
            }
            _ = sender.closed() => break,
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "codex_session_tests.rs"]
mod tests;
