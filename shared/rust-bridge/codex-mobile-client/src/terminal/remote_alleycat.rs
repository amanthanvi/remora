use std::sync::Arc;

use super::backend::OpenBackendResult;
use super::remote_shell::{RemoteShellConnection, RemoteShellTransport};
use super::session::{TerminalError, TerminalSize};

pub(crate) async fn open(
    node_id: String,
    token: String,
    relay: Option<String>,
    shell: Option<String>,
    size: TerminalSize,
) -> Result<OpenBackendResult, TerminalError> {
    let endpoint = crate::ffi::shared::shared_mobile_client()
        .alleycat_endpoint()
        .await
        .map_err(|error| TerminalError::Backend {
            detail: format!("binding remote pairing endpoint: {error}"),
        })?;
    let params = crate::alleycat::ParsedPairPayload {
        version: crate::alleycat::ALLEYCAT_PROTOCOL_VERSION,
        node_id,
        token,
        relay,
        host_name: None,
    };
    let (stream, session) =
        crate::alleycat::connect_jsonl_agent_stream(&endpoint, params, "shell".to_string())
            .await
            .map_err(map_shell_connect_error)?;
    let connection: Arc<dyn RemoteShellConnection> = Arc::new(AlleycatShellConnection { session });
    super::remote_shell::open(RemoteShellTransport::new(stream, connection), shell, size).await
}

struct AlleycatShellConnection {
    session: Arc<crate::alleycat::AlleycatSession>,
}

impl RemoteShellConnection for AlleycatShellConnection {
    fn close(&self) {
        self.session.close();
    }
}

fn map_shell_connect_error(error: crate::alleycat::AlleycatError) -> TerminalError {
    match error {
        crate::alleycat::AlleycatError::Transport(message) => {
            if is_shell_agent_unavailable_error(&message) {
                TerminalError::Backend {
                    detail: format!(
                        "Remote shell is unavailable on this paired host. Update and restart the pairing service on the host, or enable the host [agents.shell] config. Host said: {message}"
                    ),
                }
            } else {
                TerminalError::Backend {
                    detail: format!("connecting shell bridge: {message}"),
                }
            }
        }
        other => TerminalError::Backend {
            detail: format!("connecting shell bridge: {other}"),
        },
    }
}

fn is_shell_agent_unavailable_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("agent `shell` is disabled or unknown")
        || normalized.contains("agent 'shell' is disabled or unknown")
        || normalized.contains("agent shell is disabled or unknown")
        || normalized.contains("unknown agent `shell`")
        || normalized.contains("unknown agent 'shell'")
        || normalized.contains("unknown agent: shell")
}

#[cfg(test)]
mod tests {
    use super::is_shell_agent_unavailable_error;

    #[test]
    fn recognizes_alleycat_shell_agent_unavailable_errors() {
        assert!(is_shell_agent_unavailable_error(
            "agent `shell` is disabled or unknown"
        ));
        assert!(is_shell_agent_unavailable_error("unknown agent: shell"));
        assert!(!is_shell_agent_unavailable_error(
            "agent `codex` is disabled or unknown"
        ));
    }
}
