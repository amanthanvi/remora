use super::backend::OpenBackendResult;
use super::session::{TerminalError, TerminalSize};

pub(crate) async fn open(
    host_id: String,
    shell: Option<String>,
    size: TerminalSize,
) -> Result<OpenBackendResult, TerminalError> {
    let transport = crate::ffi::shared::shared_mobile_client()
        .open_remora_link_shell_transport(&host_id)
        .await?;
    super::remote_shell::open(transport, shell, size).await
}
