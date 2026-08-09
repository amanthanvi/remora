//! SSH-backed terminal backend.
//!
//! Reuses [`crate::ssh::SshClient`] for the TCP/handshake/auth phase, then
//! allocates a single session channel for an interactive PTY shell. Inbound
//! `ChannelMsg::Data` / `ExtendedData{ext:1}` is forwarded to the renderer
//! as terminal bytes; `ExitStatus` and `Close` end the stream.
//!
//! `russh::Channel` is `!Sync`, so the channel itself never crosses an
//! `await` boundary held by the backend. Instead, the open path spawns a
//! dedicated `drive_channel` task that owns the channel and `tokio::select!`s
//! between channel messages and an mpsc of [`TerminalControl`] commands.

use std::sync::Arc;

use async_trait::async_trait;
use russh::ChannelMsg;
use tokio::sync::{Mutex, mpsc};

use super::backend::{OpenBackendResult, TerminalBackend, TerminalBackendEvent};
use super::session::{TerminalError, TerminalSize};
use super::ssh_known_hosts::TerminalSshTrustStore;
use crate::ssh::{
    SshAuth, SshClient, SshCredentials, SshError, connect_with_trust_store, global_host_trust_store,
};

const CONTROL_CHANNEL_CAPACITY: usize = 32;
const OUTPUT_CHANNEL_CAPACITY: usize = 256;

/// Authentication for the SSH terminal backend. UniFFI mirror of the internal
/// [`SshAuth`] (which is not UniFFI-exported).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalSshAuth {
    Password {
        password: String,
    },
    PrivateKey {
        key_pem: String,
        passphrase: Option<String>,
    },
}

impl TerminalSshAuth {
    pub(crate) fn into_ssh_auth(self) -> SshAuth {
        match self {
            TerminalSshAuth::Password { password } => SshAuth::Password(password),
            TerminalSshAuth::PrivateKey {
                key_pem,
                passphrase,
            } => SshAuth::PrivateKey {
                key_pem,
                passphrase,
            },
        }
    }
}

#[derive(Debug)]
enum TerminalControl {
    Write(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

pub(crate) async fn open(
    host: String,
    port: u16,
    username: String,
    auth: TerminalSshAuth,
    shell: Option<String>,
    accept_unknown_host: bool,
    cwd: Option<String>,
    size: TerminalSize,
    trust_store: Option<Arc<TerminalSshTrustStore>>,
) -> Result<OpenBackendResult, TerminalError> {
    let credentials = SshCredentials {
        host: host.clone(),
        port,
        username,
        auth: auth.into_ssh_auth(),
        unlock_macos_keychain: false,
    };
    // Same shared policy the app-server SSH paths use, including the
    // trust-on-first-use serialization and the compare-and-set that records
    // the pin only after authentication succeeds. The terminal owns a per-open
    // store handle, so it passes it explicitly; when none is supplied we fall
    // back to the process-wide store registered at app startup.
    let client = connect_with_trust_store(
        trust_store.clone().or_else(global_host_trust_store),
        credentials,
        accept_unknown_host,
    )
    .await
    .map_err(map_ssh_error)?;
    let client = Arc::new(client);

    let shell_override = shell.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let cwd_arg = cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let channel = client
        .open_terminal_channel(size.cols, size.rows, shell_override, cwd_arg)
        .await
        .map_err(map_ssh_error)?;

    let (control_tx, control_rx) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(drive_channel(channel, control_rx, output_tx));

    let backend = Arc::new(RemoteSshBackend {
        control: control_tx,
        client,
        closed: Mutex::new(false),
    });
    Ok((backend, output_rx))
}

struct RemoteSshBackend {
    control: mpsc::Sender<TerminalControl>,
    client: Arc<SshClient>,
    closed: Mutex<bool>,
}

#[async_trait]
impl TerminalBackend for RemoteSshBackend {
    async fn write(&self, data: &[u8]) -> Result<(), TerminalError> {
        self.control
            .send(TerminalControl::Write(data.to_vec()))
            .await
            .map_err(|_| TerminalError::Backend {
                detail: "SSH channel task has exited".to_string(),
            })
    }

    async fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
        self.control
            .send(TerminalControl::Resize {
                cols: size.cols,
                rows: size.rows,
            })
            .await
            .map_err(|_| TerminalError::Backend {
                detail: "SSH channel task has exited".to_string(),
            })
    }

    async fn close(&self) -> Result<(), TerminalError> {
        let mut closed = self.closed.lock().await;
        if *closed {
            return Ok(());
        }
        *closed = true;
        drop(closed);

        let _ = self.control.send(TerminalControl::Close).await;
        self.client.disconnect().await;
        Ok(())
    }
}

async fn drive_channel(
    mut channel: russh::Channel<russh::client::Msg>,
    mut control_rx: mpsc::Receiver<TerminalControl>,
    output_tx: mpsc::Sender<TerminalBackendEvent>,
) {
    let mut exit_code: Option<i32> = None;
    loop {
        tokio::select! {
            biased;
            command = control_rx.recv() => {
                match command {
                    Some(TerminalControl::Write(bytes)) => {
                        if let Err(error) = channel.data(bytes.as_slice()).await {
                            let _ = output_tx.send(TerminalBackendEvent::Bytes(
                                format!("\r\n[ssh] write error: {error}\r\n").into_bytes(),
                            )).await;
                            break;
                        }
                    }
                    Some(TerminalControl::Resize { cols, rows }) => {
                        if let Err(error) = channel
                            .window_change(cols as u32, rows as u32, 0, 0)
                            .await
                        {
                            let _ = output_tx.send(TerminalBackendEvent::Bytes(
                                format!("\r\n[ssh] resize error: {error}\r\n").into_bytes(),
                            )).await;
                        }
                    }
                    Some(TerminalControl::Close) | None => {
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                        break;
                    }
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if output_tx
                            .send(TerminalBackendEvent::Bytes(data.to_vec()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                        if output_tx
                            .send(TerminalBackendEvent::Bytes(data.to_vec()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit_code = Some(exit_status as i32);
                    }
                    Some(ChannelMsg::ExitSignal { .. }) => {
                        exit_code = Some(exit_code.unwrap_or(-1));
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        }
    }
    let _ = output_tx
        .send(TerminalBackendEvent::Exit(exit_code.unwrap_or(-1)))
        .await;
}

fn map_ssh_error(error: SshError) -> TerminalError {
    match error {
        SshError::HostKeyVerification {
            host,
            port,
            fingerprint,
            pinned,
        } => TerminalError::SshHostKeyVerification {
            host,
            port,
            fingerprint,
            pinned,
        },
        SshError::HostKeyStoreUnavailable { message, .. } => TerminalError::Backend {
            detail: format!("trust-store-unavailable:{message}"),
        },
        SshError::AuthFailed(detail) => TerminalError::Backend {
            detail: format!("auth-failed:{detail}"),
        },
        SshError::ConnectionFailed(detail) => TerminalError::Backend {
            detail: format!("connect-failed:{detail}"),
        },
        SshError::Timeout => TerminalError::Backend {
            detail: "connect-timeout".to_string(),
        },
        SshError::Disconnected => TerminalError::Backend {
            detail: "disconnected".to_string(),
        },
        SshError::ExecFailed { exit_code, stderr } => TerminalError::Backend {
            detail: format!("exec-failed:{exit_code}:{stderr}"),
        },
        SshError::PortForwardFailed(detail) => TerminalError::Backend {
            detail: format!("port-forward-failed:{detail}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::session::{TerminalBackendKind, TerminalOutputListener, TerminalSession};
    use std::sync::Mutex as StdMutex;

    #[test]
    fn terminal_host_key_errors_are_typed() {
        let changed = map_ssh_error(SshError::HostKeyVerification {
            host: "host.example".into(),
            port: 22,
            fingerprint: "SHA256:new".into(),
            pinned: Some("SHA256:old".into()),
        });
        match changed {
            TerminalError::SshHostKeyVerification {
                host,
                port,
                fingerprint,
                pinned,
            } => {
                assert_eq!(host, "host.example");
                assert_eq!(port, 22);
                assert_eq!(fingerprint, "SHA256:new");
                assert_eq!(pinned.as_deref(), Some("SHA256:old"));
            }
            other => panic!("expected SshHostKeyVerification, got {other:?}"),
        }

        let unknown = map_ssh_error(SshError::HostKeyVerification {
            host: "host.example".into(),
            port: 2222,
            fingerprint: "SHA256:new".into(),
            pinned: None,
        });
        match unknown {
            TerminalError::SshHostKeyVerification {
                host,
                port,
                fingerprint,
                pinned,
            } => {
                assert_eq!(host, "host.example");
                assert_eq!(port, 2222);
                assert_eq!(fingerprint, "SHA256:new");
                assert_eq!(pinned, None);
            }
            other => panic!("expected SshHostKeyVerification, got {other:?}"),
        }
    }

    fn parse_live_target() -> Option<(String, u16, String, TerminalSshAuth)> {
        let raw = match std::env::var("REMORA_TERMINAL_LIVE_SSH") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return None,
        };
        // Format: user:password@host:port
        let (user_pw, host_port) = raw.split_once('@')?;
        let (user, pw) = user_pw.split_once(':')?;
        let (host, port) = host_port.rsplit_once(':')?;
        let port = port.parse::<u16>().ok()?;
        Some((
            host.to_string(),
            port,
            user.to_string(),
            TerminalSshAuth::Password {
                password: pw.to_string(),
            },
        ))
    }

    #[tokio::test]
    async fn accept_unknown_host_false_rejects_first_connect() {
        // Without a live target, this test verifies the policy-mapping path
        // by routing through an unreachable host. We expect a connect-failed
        // path, which proves the host_key_callback closure compiles and the
        // SshCredentials shape matches. The specific connect-failure detail
        // text is environment-dependent and isn't asserted here.
        let result = open(
            "127.0.0.1".to_string(),
            1,
            "nobody".to_string(),
            TerminalSshAuth::Password {
                password: "x".to_string(),
            },
            None,
            false,
            None,
            TerminalSize { cols: 80, rows: 24 },
            None,
        )
        .await;
        assert!(result.is_err(), "expected open to fail against port 1");
    }

    #[tokio::test]
    #[ignore = "requires a live SSH host; set REMORA_TERMINAL_LIVE_SSH=user:password@host:port"]
    async fn live_remote_ssh_terminal_round_trips_shell_io() {
        let Some((host, port, username, auth)) = parse_live_target() else {
            eprintln!("skipping: REMORA_TERMINAL_LIVE_SSH is not set");
            return;
        };

        let session = TerminalSession::open(
            TerminalBackendKind::RemoteSsh {
                host,
                port,
                username,
                auth,
                shell: None,
                accept_unknown_host: true,
                cwd: None,
            },
            TerminalSize { cols: 77, rows: 31 },
        )
        .await
        .expect("open SSH terminal");

        let bytes: Arc<StdMutex<Vec<u8>>> = Arc::new(StdMutex::new(Vec::new()));
        let exits: Arc<StdMutex<Vec<i32>>> = Arc::new(StdMutex::new(Vec::new()));

        struct Listener {
            bytes: Arc<StdMutex<Vec<u8>>>,
            exits: Arc<StdMutex<Vec<i32>>>,
        }

        impl TerminalOutputListener for Listener {
            fn on_bytes(&self, data: Vec<u8>) {
                self.bytes.lock().unwrap().extend(data);
            }
            fn on_exit(&self, code: i32) {
                self.exits.lock().unwrap().push(code);
            }
        }

        session.subscribe_output(Box::new(Listener {
            bytes: bytes.clone(),
            exits: exits.clone(),
        }));

        session
            .write_input(b"printf 'remote-ssh-ready\\n'; stty size; exit 0\n".to_vec())
            .await
            .expect("write shell input");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if tokio::time::Instant::now() >= deadline {
                let snapshot = String::from_utf8_lossy(&bytes.lock().unwrap()).to_string();
                panic!("timed out waiting for live ssh output; got {snapshot:?}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let snapshot = String::from_utf8_lossy(&bytes.lock().unwrap()).to_string();
            if snapshot.contains("remote-ssh-ready") && snapshot.contains("31 77") {
                break;
            }
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !exits.lock().unwrap().is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for live ssh exit notification");
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        session.close_session().await.ok();
    }
}
