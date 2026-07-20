use crate::types::{AgentRuntimeKind, ThreadKey};

use super::TerminalBackendKind;

/// Non-secret description of the transport that owns a terminal session.
///
/// The credential-bearing [`TerminalBackendKind`] is an input-only value. It
/// must never be retained in app snapshots or projected through UniFFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalTransportKind {
    RemoraLink,
    Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalTransportDescriptor {
    pub kind: TerminalTransportKind,
    pub display_label: String,
}

impl TerminalTransportDescriptor {
    pub(crate) fn from_backend(backend: &TerminalBackendKind) -> Self {
        match backend {
            TerminalBackendKind::RemoteAlleycat { .. }
            | TerminalBackendKind::RemoteRemoraLink { .. } => Self {
                kind: TerminalTransportKind::RemoraLink,
                // Deliberately omit the node id and relay. They are routing
                // material, not presentation state, and can be correlated
                // with a credential-bearing pairing record.
                display_label: "Remora Link".to_string(),
            },
            TerminalBackendKind::RemoteSsh {
                host,
                port,
                username,
                ..
            } => Self {
                kind: TerminalTransportKind::Ssh,
                display_label: format!("{username}@{host}:{port}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TerminalContextCapabilities {
    pub can_open: bool,
    pub can_attach: bool,
    pub can_write: bool,
    pub can_resize: bool,
    pub can_close: bool,
}

impl TerminalContextCapabilities {
    pub(crate) const fn unavailable() -> Self {
        Self {
            can_open: false,
            can_attach: false,
            can_write: false,
            can_resize: false,
            can_close: false,
        }
    }

    pub(crate) const fn for_unopened_thread(can_open: bool) -> Self {
        Self {
            can_open,
            can_attach: false,
            can_write: false,
            can_resize: false,
            can_close: false,
        }
    }

    pub(crate) const fn for_live_session() -> Self {
        Self {
            can_open: true,
            can_attach: true,
            can_write: true,
            can_resize: true,
            can_close: true,
        }
    }
}

/// Canonical Rust-owned identity for a terminal route.
///
/// `thread_key == None` is an explicit server-wide terminal. It must never be
/// silently treated as belonging to the currently visible thread.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadTerminalContext {
    pub thread_key: Option<ThreadKey>,
    pub terminal_session_id: Option<String>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub agent_runtime_kind: Option<AgentRuntimeKind>,
    pub transport: Option<TerminalTransportDescriptor>,
    pub capabilities: TerminalContextCapabilities,
}

impl ThreadTerminalContext {
    pub(crate) fn unscoped_session(
        terminal_session_id: String,
        transport: TerminalTransportDescriptor,
    ) -> Self {
        Self {
            thread_key: None,
            terminal_session_id: Some(terminal_session_id),
            cwd: None,
            worktree_path: None,
            agent_runtime_kind: None,
            transport: Some(transport),
            capabilities: TerminalContextCapabilities::for_live_session(),
        }
    }

    pub(crate) fn belongs_to(&self, key: &ThreadKey, terminal_session_id: &str) -> bool {
        self.thread_key.as_ref() == Some(key)
            && self.terminal_session_id.as_deref() == Some(terminal_session_id)
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ThreadTerminalContextError {
    #[error("Unknown thread: {server_id}/{thread_id}")]
    UnknownThread {
        server_id: String,
        thread_id: String,
    },
    #[error("Unknown terminal session: {terminal_session_id}")]
    UnknownTerminalSession { terminal_session_id: String },
    #[error("Terminal session does not belong to the requested thread")]
    ContextMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalSshAuth;

    #[test]
    fn paired_host_descriptor_does_not_retain_routing_or_credentials() {
        let backend = TerminalBackendKind::RemoteAlleycat {
            node_id: "secret-node".to_string(),
            token: "secret-token".to_string(),
            relay: Some("secret-relay".to_string()),
            shell: Some("/bin/zsh".to_string()),
        };

        let descriptor = TerminalTransportDescriptor::from_backend(&backend);
        let debug = format!("{descriptor:?}");

        assert_eq!(descriptor.kind, TerminalTransportKind::RemoraLink);
        assert_eq!(descriptor.display_label, "Remora Link");
        assert!(!debug.contains("secret-node"));
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("secret-relay"));
    }

    #[test]
    fn v2_paired_host_descriptor_retains_only_the_nonsecret_transport_label() {
        let backend = TerminalBackendKind::RemoteRemoraLink {
            host_id: "remora-link:sensitive-endpoint-id".to_string(),
            shell: Some("/bin/zsh".to_string()),
        };

        let descriptor = TerminalTransportDescriptor::from_backend(&backend);
        let backend_debug = format!("{backend:?}");
        let descriptor_debug = format!("{descriptor:?}");

        assert_eq!(descriptor.kind, TerminalTransportKind::RemoraLink);
        assert_eq!(descriptor.display_label, "Remora Link");
        assert!(backend_debug.contains("remora-link:sensitive-endpoint-id"));
        assert!(!descriptor_debug.contains("sensitive-endpoint-id"));
        assert!(!descriptor_debug.contains("/bin/zsh"));
    }

    #[test]
    fn ssh_descriptor_does_not_retain_auth_material() {
        let backend = TerminalBackendKind::RemoteSsh {
            host: "host.example".to_string(),
            port: 22,
            username: "aman".to_string(),
            auth: TerminalSshAuth::Password {
                password: "secret-password".to_string(),
            },
            shell: None,
            accept_unknown_host: false,
            cwd: None,
        };

        let descriptor = TerminalTransportDescriptor::from_backend(&backend);
        let debug = format!("{descriptor:?}");

        assert_eq!(descriptor.kind, TerminalTransportKind::Ssh);
        assert_eq!(descriptor.display_label, "aman@host.example:22");
        assert!(!debug.contains("secret-password"));
    }
}
