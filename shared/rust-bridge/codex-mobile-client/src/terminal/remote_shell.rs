use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::backend::{OpenBackendResult, TerminalBackend, TerminalBackendEvent};
use super::session::{TerminalError, TerminalSize};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
type PendingResponses = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

pub(crate) trait RemoteShellIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> RemoteShellIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub(crate) trait RemoteShellConnection: Send + Sync {
    fn close(&self);
}

pub(crate) struct RemoteShellTransport {
    stream: Box<dyn RemoteShellIo>,
    connection: RemoteShellConnectionGuard,
}

impl RemoteShellTransport {
    pub(crate) fn new(
        stream: impl RemoteShellIo + 'static,
        connection: Arc<dyn RemoteShellConnection>,
    ) -> Self {
        Self {
            stream: Box::new(stream),
            connection: RemoteShellConnectionGuard { connection },
        }
    }
}

struct RemoteShellConnectionGuard {
    connection: Arc<dyn RemoteShellConnection>,
}

impl RemoteShellConnectionGuard {
    fn close(&self) {
        self.connection.close();
    }
}

impl Drop for RemoteShellConnectionGuard {
    fn drop(&mut self) {
        self.connection.close();
    }
}

pub(crate) async fn open(
    transport: RemoteShellTransport,
    shell: Option<String>,
    size: TerminalSize,
) -> Result<OpenBackendResult, TerminalError> {
    let RemoteShellTransport { stream, connection } = transport;
    let (reader, mut writer) = tokio::io::split(stream);
    let reader = BufReader::new(reader);
    let (output_tx, output_rx) = mpsc::channel(256);
    let pending_responses = Arc::new(Mutex::new(HashMap::new()));
    let shell_session_id = Arc::new(Mutex::new(None));

    tokio::spawn(read_output_loop(
        reader,
        Arc::clone(&shell_session_id),
        output_tx,
        Arc::clone(&pending_responses),
    ));

    let _: Value = tracked_request(
        &mut writer,
        &pending_responses,
        1,
        "initialize",
        json!({
            "clientInfo": { "name": "Remora", "version": "1.0" },
            "capabilities": { "experimentalApi": true }
        }),
    )
    .await?;

    let spawn_response: ShellSpawnResponse = deserialize_result(
        tracked_request(
            &mut writer,
            &pending_responses,
            2,
            "shell/spawn",
            json!({
                "shell": shell,
                "size": { "cols": size.cols, "rows": size.rows }
            }),
        )
        .await?,
        "shell/spawn",
    )?;
    *shell_session_id.lock().await = Some(spawn_response.session_id.clone());

    let backend = Arc::new(RemoteShellBackend {
        writer: Mutex::new(writer),
        connection,
        shell_session_id: spawn_response.session_id,
        next_request_id: AtomicI64::new(3),
        pending_responses,
        closed: AtomicBool::new(false),
    });
    Ok((backend, output_rx))
}

async fn tracked_request<W>(
    writer: &mut W,
    pending_responses: &PendingResponses,
    request_id: i64,
    method: &str,
    params: Value,
) -> Result<Value, TerminalError>
where
    W: AsyncWrite + Unpin,
{
    let (tx, rx) = oneshot::channel();
    pending_responses.lock().await.insert(request_id, tx);
    if let Err(error) = send_request(writer, request_id, method, params).await {
        pending_responses.lock().await.remove(&request_id);
        return Err(error);
    }
    await_response(pending_responses, request_id, method, rx).await
}

fn deserialize_result<T>(result: Value, method: &str) -> Result<T, TerminalError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(result).map_err(|error| TerminalError::Backend {
        detail: format!("decoding terminal JSON-RPC result for {method}: {error}"),
    })
}

async fn await_response(
    pending_responses: &PendingResponses,
    request_id: i64,
    method: &str,
    rx: oneshot::Receiver<Result<Value, String>>,
) -> Result<Value, TerminalError> {
    match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
        Ok(Ok(Ok(result))) => Ok(result),
        Ok(Ok(Err(message))) => Err(TerminalError::Backend { detail: message }),
        Ok(Err(_)) => Err(TerminalError::Backend {
            detail: "terminal JSON-RPC response channel closed".to_string(),
        }),
        Err(_) => {
            pending_responses.lock().await.remove(&request_id);
            Err(TerminalError::Backend {
                detail: format!("timed out waiting for terminal JSON-RPC response to {method}"),
            })
        }
    }
}

struct RemoteShellBackend {
    writer: Mutex<WriteHalf<Box<dyn RemoteShellIo>>>,
    connection: RemoteShellConnectionGuard,
    shell_session_id: String,
    next_request_id: AtomicI64,
    pending_responses: PendingResponses,
    closed: AtomicBool,
}

#[async_trait]
impl TerminalBackend for RemoteShellBackend {
    async fn write(&self, data: &[u8]) -> Result<(), TerminalError> {
        self.request(
            "shell/input",
            json!({
                "session_id": self.shell_session_id,
                "data_b64": STANDARD.encode(data),
            }),
        )
        .await?;
        Ok(())
    }

    async fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
        self.request(
            "shell/resize",
            json!({
                "session_id": self.shell_session_id,
                "cols": size.cols,
                "rows": size.rows,
            }),
        )
        .await?;
        Ok(())
    }

    async fn close(&self) -> Result<(), TerminalError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _close_on_cancel = CloseOnDrop(&self.connection);
        let request_id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.pending_responses.lock().await.insert(request_id, tx);
        let send_result = {
            let mut writer = self.writer.lock().await;
            send_request(
                &mut *writer,
                request_id,
                "shell/kill",
                json!({ "session_id": self.shell_session_id }),
            )
            .await
        };
        if let Err(error) = send_result {
            self.pending_responses.lock().await.remove(&request_id);
            return Err(error);
        }
        await_response(&self.pending_responses, request_id, "shell/kill", rx)
            .await
            .map(|_| ())
    }
}

struct CloseOnDrop<'a>(&'a RemoteShellConnectionGuard);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl RemoteShellBackend {
    fn next_id(&self) -> i64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, TerminalError> {
        let request_id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.pending_responses.lock().await.insert(request_id, tx);

        let send_result = {
            let mut writer = self.writer.lock().await;
            send_request(&mut *writer, request_id, method, params).await
        };
        if let Err(error) = send_result {
            self.pending_responses.lock().await.remove(&request_id);
            return Err(error);
        }

        await_response(&self.pending_responses, request_id, method, rx).await
    }
}

async fn read_output_loop(
    mut reader: BufReader<ReadHalf<Box<dyn RemoteShellIo>>>,
    shell_session_id: Arc<Mutex<Option<String>>>,
    output_tx: mpsc::Sender<TerminalBackendEvent>,
    pending_responses: PendingResponses,
) {
    let close_reason = loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                let _ = output_tx.send(TerminalBackendEvent::Exit(-1)).await;
                break "terminal JSON-RPC stream closed".to_string();
            }
            Ok(_) => {}
            Err(error) => {
                let _ = output_tx.send(TerminalBackendEvent::Exit(-1)).await;
                break format!("reading terminal JSON-RPC stream: {error}");
            }
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = frame.get("id").and_then(Value::as_i64) {
            complete_pending_response(&pending_responses, id, response_payload(frame)).await;
            continue;
        }
        match frame.get("method").and_then(Value::as_str) {
            Some("shell/output") => {
                let Some(params) = frame.get("params") else {
                    continue;
                };
                let Ok(params) = serde_json::from_value::<ShellOutputNotification>(params.clone())
                else {
                    continue;
                };
                if !should_accept_session(&shell_session_id, &params.session_id).await {
                    continue;
                }
                let Ok(bytes) = STANDARD.decode(params.data_b64.as_bytes()) else {
                    continue;
                };
                if output_tx
                    .send(TerminalBackendEvent::Bytes(bytes))
                    .await
                    .is_err()
                {
                    break "terminal output listener closed".to_string();
                }
            }
            Some("shell/exit") => {
                let Some(params) = frame.get("params") else {
                    continue;
                };
                let Ok(params) = serde_json::from_value::<ShellExitNotification>(params.clone())
                else {
                    continue;
                };
                if !should_accept_session(&shell_session_id, &params.session_id).await {
                    continue;
                }
                let _ = output_tx
                    .send(TerminalBackendEvent::Exit(params.code))
                    .await;
                break format!("terminal shell session exited with code {}", params.code);
            }
            _ => {}
        }
    };
    fail_pending_responses(&pending_responses, close_reason).await;
}

async fn should_accept_session(
    expected_session_id: &Arc<Mutex<Option<String>>>,
    actual_session_id: &str,
) -> bool {
    match expected_session_id.lock().await.as_deref() {
        Some(expected) => expected == actual_session_id,
        None => true,
    }
}

async fn send_request<W>(
    writer: &mut W,
    id: i64,
    method: &str,
    params: Value,
) -> Result<(), TerminalError>
where
    W: AsyncWrite + Unpin,
{
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_vec(&frame).map_err(|error| TerminalError::Backend {
        detail: format!("encoding terminal JSON-RPC request: {error}"),
    })?;
    writer
        .write_all(&line)
        .await
        .map_err(|error| TerminalError::Backend {
            detail: format!("writing terminal JSON-RPC request: {error}"),
        })?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|error| TerminalError::Backend {
            detail: format!("writing terminal JSON-RPC newline: {error}"),
        })?;
    writer
        .flush()
        .await
        .map_err(|error| TerminalError::Backend {
            detail: format!("flushing terminal JSON-RPC request: {error}"),
        })
}

fn response_payload(frame: Value) -> Result<Value, String> {
    if let Some(error) = frame.get("error") {
        return Err(format!("terminal JSON-RPC error: {error}"));
    }
    frame
        .get("result")
        .cloned()
        .ok_or_else(|| "terminal JSON-RPC response missing result".to_string())
}

async fn complete_pending_response(
    pending_responses: &PendingResponses,
    id: i64,
    payload: Result<Value, String>,
) {
    if let Some(tx) = pending_responses.lock().await.remove(&id) {
        let _ = tx.send(payload);
    }
}

async fn fail_pending_responses(pending_responses: &PendingResponses, detail: String) {
    let pending = {
        let mut pending = pending_responses.lock().await;
        std::mem::take(&mut *pending)
    };
    for (_, tx) in pending {
        let _ = tx.send(Err(detail.clone()));
    }
}

#[derive(Debug, Deserialize)]
struct ShellSpawnResponse {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct ShellOutputNotification {
    session_id: String,
    data_b64: String,
}

#[derive(Debug, Deserialize)]
struct ShellExitNotification {
    session_id: String,
    code: i32,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    use super::*;

    struct TestConnection {
        closed: Arc<AtomicBool>,
    }

    impl RemoteShellConnection for TestConnection {
        fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    async fn read_frame(reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write_frame(writer: &mut tokio::io::WriteHalf<DuplexStream>, frame: Value) {
        writer
            .write_all(serde_json::to_string(&frame).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn shared_shell_protocol_drives_initialize_spawn_io_resize_and_kill() {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (server_reader, mut server_writer) = tokio::io::split(server);
        let mut server_reader = BufReader::new(server_reader);
        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            assert_eq!(initialize["method"], "initialize");
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":1,"result":{}}),
            )
            .await;

            let spawn = read_frame(&mut server_reader).await;
            assert_eq!(spawn["method"], "shell/spawn");
            assert_eq!(spawn["params"]["shell"], "/bin/zsh");
            assert_eq!(spawn["params"]["size"], json!({"cols":80,"rows":24}));
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":2,"result":{"session_id":"shell-1"}}),
            )
            .await;
            write_frame(
                &mut server_writer,
                json!({
                    "jsonrpc":"2.0",
                    "method":"shell/output",
                    "params":{"session_id":"shell-1","data_b64":"aGVsbG8="}
                }),
            )
            .await;

            let input = read_frame(&mut server_reader).await;
            assert_eq!(input["method"], "shell/input");
            assert_eq!(input["params"]["session_id"], "shell-1");
            assert_eq!(input["params"]["data_b64"], "d29ybGQ=");
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":input["id"],"result":{}}),
            )
            .await;

            let resize = read_frame(&mut server_reader).await;
            assert_eq!(resize["method"], "shell/resize");
            assert_eq!(resize["params"]["cols"], 120);
            assert_eq!(resize["params"]["rows"], 40);
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":resize["id"],"result":{}}),
            )
            .await;

            let kill = read_frame(&mut server_reader).await;
            assert_eq!(kill["method"], "shell/kill");
            assert_eq!(kill["params"]["session_id"], "shell-1");
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":kill["id"],"result":{}}),
            )
            .await;
        });
        let closed = Arc::new(AtomicBool::new(false));
        let connection: Arc<dyn RemoteShellConnection> = Arc::new(TestConnection {
            closed: Arc::clone(&closed),
        });
        let (backend, mut output) = open(
            RemoteShellTransport::new(client, connection),
            Some("/bin/zsh".to_string()),
            TerminalSize { cols: 80, rows: 24 },
        )
        .await
        .unwrap();

        assert!(matches!(
            output.recv().await,
            Some(TerminalBackendEvent::Bytes(bytes)) if bytes == b"hello"
        ));
        backend.write(b"world").await.unwrap();
        backend
            .resize(TerminalSize {
                cols: 120,
                rows: 40,
            })
            .await
            .unwrap();
        backend.close().await.unwrap();
        assert!(closed.load(Ordering::SeqCst));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_open_or_dropping_backend_closes_transport_lifetime() {
        let (client, server) = tokio::io::duplex(4096);
        let (server_reader, _server_writer) = tokio::io::split(server);
        let mut server_reader = BufReader::new(server_reader);
        let closed = Arc::new(AtomicBool::new(false));
        let connection: Arc<dyn RemoteShellConnection> = Arc::new(TestConnection {
            closed: Arc::clone(&closed),
        });
        let opening = tokio::spawn(open(
            RemoteShellTransport::new(client, connection),
            None,
            TerminalSize { cols: 80, rows: 24 },
        ));
        let initialize = read_frame(&mut server_reader).await;
        assert_eq!(initialize["method"], "initialize");
        opening.abort();
        assert!(matches!(opening.await, Err(error) if error.is_cancelled()));
        tokio::task::yield_now().await;
        assert!(closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_an_open_backend_closes_transport_without_an_explicit_kill() {
        let (client, server) = tokio::io::duplex(4096);
        let (server_reader, mut server_writer) = tokio::io::split(server);
        let mut server_reader = BufReader::new(server_reader);
        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":initialize["id"],"result":{}}),
            )
            .await;
            let spawn = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":spawn["id"],"result":{"session_id":"shell-drop"}}),
            )
            .await;
        });
        let closed = Arc::new(AtomicBool::new(false));
        let connection: Arc<dyn RemoteShellConnection> = Arc::new(TestConnection {
            closed: Arc::clone(&closed),
        });
        let (backend, _output) = open(
            RemoteShellTransport::new(client, connection),
            None,
            TerminalSize { cols: 80, rows: 24 },
        )
        .await
        .unwrap();
        server.await.unwrap();

        drop(backend);
        tokio::task::yield_now().await;
        assert!(closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelling_close_after_kill_send_still_closes_and_retry_is_a_noop() {
        let (client, server) = tokio::io::duplex(4096);
        let (server_reader, mut server_writer) = tokio::io::split(server);
        let mut server_reader = BufReader::new(server_reader);
        let (kill_seen_tx, kill_seen_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":initialize["id"],"result":{}}),
            )
            .await;
            let spawn = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_writer,
                json!({"jsonrpc":"2.0","id":spawn["id"],"result":{"session_id":"shell-close"}}),
            )
            .await;
            let kill = read_frame(&mut server_reader).await;
            assert_eq!(kill["method"], "shell/kill");
            let _ = kill_seen_tx.send(());
            std::future::pending::<()>().await;
        });
        let closed = Arc::new(AtomicBool::new(false));
        let connection: Arc<dyn RemoteShellConnection> = Arc::new(TestConnection {
            closed: Arc::clone(&closed),
        });
        let (backend, _output) = open(
            RemoteShellTransport::new(client, connection),
            None,
            TerminalSize { cols: 80, rows: 24 },
        )
        .await
        .unwrap();

        let closing_backend = Arc::clone(&backend);
        let closing = tokio::spawn(async move { closing_backend.close().await });
        kill_seen_rx.await.unwrap();
        assert!(!closed.load(Ordering::SeqCst));
        closing.abort();
        assert!(matches!(closing.await, Err(error) if error.is_cancelled()));
        assert!(closed.load(Ordering::SeqCst));
        backend.close().await.unwrap();
        server.abort();
    }
}
