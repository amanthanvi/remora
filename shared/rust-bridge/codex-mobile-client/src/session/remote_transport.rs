//! Transport-agnostic reconnect surface for remote `AppServerClient`s.
//!
//! SSH, Remora Link, and other managed transports need to be able to
//! re-establish their underlying connection
//! and produce a fresh `AppServerClient` after a transient drop. This module
//! defines the small trait the session worker uses to drive that operation
//! without knowing which transport is underneath.

use std::sync::Arc;

use async_trait::async_trait;
use codex_app_server_client::{AppServerClient, RemoteAppServerConnectArgs};

use crate::transport::TransportError;

/// Whether the transport's replay handshake proved that the replacement
/// stream is caught up. `AuthoritativeRefreshRequired` is deliberately a
/// transport-neutral outcome for reconnect protocols that detect replay
/// drift, while transports without replay semantics retain the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ReplayOutcome {
    #[default]
    Complete,
    AuthoritativeRefreshRequired,
}

/// Transport-scoped state that must outlive the worker's `client` binding
/// (e.g. a connection backing a managed stream). The worker
/// swaps the keepalive Arc on each successful reconnect so the previous
/// resource is dropped only AFTER the new one is installed.
///
/// The `close()` hook is called once before the worker drops the
/// keepalive on graceful teardown (`SessionCommand::Shutdown` or
/// `disconnect()`), so transports can issue a typed close frame to the
/// peer. Default impl is a no-op for transports (e.g. SSH) where the
/// underlying TCP stream is closed by `AppServerClient::shutdown`.
pub(crate) trait SessionKeepalive: Send + Sync + 'static {
    /// Send a graceful close to the peer. May be called concurrently with
    /// the worker's own teardown — implementations must be idempotent and
    /// non-panicking.
    fn close(&self) {}
}

/// Result of a successful reconnect.
pub(crate) struct Reconnected {
    /// Fresh client wired to the newly-established transport.
    pub client: AppServerClient,
    /// Optional transport-scoped resource (see [`SessionKeepalive`]). The
    /// worker keeps the previous keepalive alive until the new client is
    /// installed; on graceful shutdown it calls `close()` before dropping.
    pub keepalive: Option<Arc<dyn SessionKeepalive>>,
}

/// Reconnect strategy for a remote `AppServerClient`.
///
/// Implementations are held by the session worker as `Arc<dyn RemoteTransport>`.
/// Reconnect runs only when the existing client drops (cold path), so dynamic
/// dispatch overhead is irrelevant compared to the network handshake cost.
#[async_trait]
pub(crate) trait RemoteTransport: Send + Sync + 'static {
    /// Re-establish the underlying transport and return a fresh client.
    ///
    /// `args` and `websocket_url` describe the original connect parameters and
    /// are provided for transports that fall back to a plain WebSocket connect
    /// (e.g. SSH after a port-forward refresh). Transports that derive their
    /// route from their own parameters may ignore them.
    async fn reconnect(
        &self,
        args: &RemoteAppServerConnectArgs,
        websocket_url: &str,
    ) -> Result<Reconnected, TransportError>;

    /// Low-cardinality route label for local connection timelines. It must
    /// never contain a host, URL, token, account, or other user data.
    fn route_label(&self) -> &'static str {
        "managed"
    }

    /// Return and clear the replay outcome produced by the most recent
    /// successful reconnect. Implementations that do not negotiate replay
    /// completeness use the default `Complete` outcome.
    fn take_replay_outcome(&self) -> ReplayOutcome {
        ReplayOutcome::Complete
    }

    /// Complete any authoritative reconciliation required by the replay
    /// outcome before the session publishes `Connected`.
    async fn reconcile_replay(
        &self,
        _client: &AppServerClient,
        _outcome: ReplayOutcome,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    /// Hint that the host network may have changed (e.g. iOS resumed the
    /// app from background suspension). Transports that have an iroh
    /// `Endpoint` use this to call `Endpoint::network_change()` so QUIC
    /// can re-probe paths and refresh relay connections without waiting
    /// for the idle timeout. Default: no-op for TCP-based transports
    /// where the OS already surfaces network changes.
    async fn notify_network_change(&self) {}
}

#[cfg(test)]
mod tests {
    //! Tests for the `RemoteTransport` contract surface.
    //!
    //! Exercising `reconnect_remote_client` end-to-end requires constructing a
    //! real `AppServerClient`, which has no public test constructor. So these
    //! tests cover the parts that are testable in isolation: drop ordering of
    //! the keepalive slot the worker maintains, and the trait being usable as
    //! `Arc<dyn RemoteTransport>`.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DropCounter {
        counter: Arc<AtomicUsize>,
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl SessionKeepalive for DropCounter {}

    /// The worker maintains `let mut keepalive: Option<Arc<dyn SessionKeepalive>>`
    /// and on a successful reconnect runs `*keepalive = next.keepalive`.
    /// Verify that assigning a new `Some(...)` drops the previous Arc, and
    /// that holding `None` leaves the previous Arc untouched.
    #[test]
    fn keepalive_assignment_drops_previous_arc() {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let initial: Arc<dyn SessionKeepalive> = Arc::new(DropCounter {
            counter: Arc::clone(&drop_count),
        });
        let mut keepalive: Option<Arc<dyn SessionKeepalive>> = Some(Arc::clone(&initial));

        // Drop the local strong reference; only `keepalive` keeps it alive now.
        drop(initial);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);

        // SSH-style reconnect: keepalive is None, slot must NOT be cleared.
        let next_ssh: Option<Arc<dyn SessionKeepalive>> = None;
        if next_ssh.is_some() {
            keepalive = next_ssh;
        }
        assert_eq!(
            drop_count.load(Ordering::SeqCst),
            0,
            "an SSH reconnect (keepalive: None) must not clobber the existing keepalive"
        );

        // Managed reconnect: keepalive carries a fresh Arc; old one drops.
        let next_managed_drop_count = Arc::new(AtomicUsize::new(0));
        let next_managed: Arc<dyn SessionKeepalive> = Arc::new(DropCounter {
            counter: Arc::clone(&next_managed_drop_count),
        });
        keepalive = Some(next_managed);
        assert_eq!(
            drop_count.load(Ordering::SeqCst),
            1,
            "the previous keepalive Arc must drop exactly once after the swap"
        );
        assert_eq!(
            next_managed_drop_count.load(Ordering::SeqCst),
            0,
            "the new keepalive Arc must remain alive"
        );

        // Worker exit: dropping the slot drops the new keepalive.
        drop(keepalive);
        assert_eq!(next_managed_drop_count.load(Ordering::SeqCst), 1);
    }

    /// `RemoteTransport` must be usable as a trait object (`Arc<dyn ...>`).
    /// This compiles iff the trait stays object-safe.
    #[test]
    fn trait_is_object_safe() {
        struct FakeTransport;

        #[async_trait]
        impl RemoteTransport for FakeTransport {
            async fn reconnect(
                &self,
                _args: &RemoteAppServerConnectArgs,
                _websocket_url: &str,
            ) -> Result<Reconnected, TransportError> {
                Err(TransportError::ConnectionFailed("test".into()))
            }
        }

        let _t: Arc<dyn RemoteTransport> = Arc::new(FakeTransport);
    }
}
