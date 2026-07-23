//! Take-once custody and app-server adaptation for attached v2 runtime streams.
//!
//! The control lifecycle authenticates and attaches a runtime stream. This
//! module keeps the resulting byte stream transport-neutral: attachment
//! identity is exact, custody is bounded and expiring, and the stream adapter
//! selects the advertised app-server framing without reusing retired v1 state.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use codex_app_server_client::{AppServerClient, RemoteAppServerClient, RemoteAppServerConnectArgs};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::AgentWireV2;
use crate::transport::TransportError;

const MAX_OBSERVED_FRAME_BYTES: usize = 1024 * 1024;
const REMORA_LINK_SEQUENCE_FIELD: &str = "_remora_link_seq";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFramingV2 {
    Websocket,
    Jsonl,
}

fn runtime_framing(wire: AgentWireV2) -> RuntimeFramingV2 {
    match wire {
        AgentWireV2::Websocket => RuntimeFramingV2::Websocket,
        AgentWireV2::Jsonl => RuntimeFramingV2::Jsonl,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AttachmentCustodyErrorV2 {
    #[error("runtime attachment is unavailable")]
    Unavailable,
    #[error("runtime attachment identity does not match")]
    IdentityMismatch,
    #[error("runtime attachment identity was reused")]
    DuplicateIdentity,
}

struct RetainedAttachmentV2<T> {
    host_id: String,
    runtime_id: String,
    retained_at: Instant,
    value: T,
}

/// Process-local custody for attached runtime streams.
///
/// IDs are supplied by the authenticated host adapter and are the sole lookup
/// key. Host/runtime are checked before removal, so a mismatched caller cannot
/// consume another concurrent attachment. Values removed by expiry, eviction,
/// or host cleanup are dropped immediately, allowing their transport owner to
/// close the underlying connection.
pub(crate) struct RetainedAttachmentRegistryV2<T> {
    entries: HashMap<String, RetainedAttachmentV2<T>>,
    max_entries: usize,
    max_age: Duration,
}

impl<T> RetainedAttachmentRegistryV2<T> {
    pub(crate) fn new(max_entries: usize, max_age: Duration) -> Self {
        assert!(
            max_entries > 0,
            "attachment custody must be bounded above zero"
        );
        Self {
            entries: HashMap::new(),
            max_entries,
            max_age,
        }
    }

    pub(crate) fn insert(
        &mut self,
        attachment_id: String,
        host_id: String,
        runtime_id: String,
        value: T,
        now: Instant,
    ) -> Result<(), AttachmentCustodyErrorV2> {
        self.prune_expired(now);
        if self.entries.contains_key(&attachment_id) {
            return Err(AttachmentCustodyErrorV2::DuplicateIdentity);
        }
        while self.entries.len() >= self.max_entries {
            let oldest_id = self
                .entries
                .iter()
                .min_by(|(left_id, left), (right_id, right)| {
                    left.retained_at
                        .cmp(&right.retained_at)
                        .then_with(|| left_id.cmp(right_id))
                })
                .map(|(id, _)| id.clone())
                .expect("non-empty registry at capacity");
            self.entries.remove(&oldest_id);
        }
        self.entries.insert(
            attachment_id,
            RetainedAttachmentV2 {
                host_id,
                runtime_id,
                retained_at: now,
                value,
            },
        );
        Ok(())
    }

    pub(crate) fn take(
        &mut self,
        attachment_id: &str,
        host_id: &str,
        runtime_id: &str,
        now: Instant,
    ) -> Result<T, AttachmentCustodyErrorV2> {
        self.prune_expired(now);
        let retained = self
            .entries
            .get(attachment_id)
            .ok_or(AttachmentCustodyErrorV2::Unavailable)?;
        if retained.host_id != host_id || retained.runtime_id != runtime_id {
            return Err(AttachmentCustodyErrorV2::IdentityMismatch);
        }
        Ok(self
            .entries
            .remove(attachment_id)
            .expect("attachment remained present after identity check")
            .value)
    }

    pub(crate) fn remove_host(&mut self, host_id: &str) {
        self.entries
            .retain(|_, retained| retained.host_id != host_id);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        self.prune_expired(now);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn prune_expired(&mut self, now: Instant) {
        self.entries.retain(|_, retained| {
            now.checked_duration_since(retained.retained_at)
                .is_some_and(|age| age < self.max_age)
        });
    }
}

#[derive(Debug)]
enum SequenceObserverV2 {
    Jsonl(JsonlSequenceObserverV2),
    Websocket(WebsocketSequenceObserverV2),
}

impl SequenceObserverV2 {
    fn new(wire: AgentWireV2) -> Self {
        match wire {
            AgentWireV2::Jsonl => Self::Jsonl(JsonlSequenceObserverV2::default()),
            AgentWireV2::Websocket => Self::Websocket(WebsocketSequenceObserverV2::default()),
        }
    }

    fn observe(&mut self, bytes: &[u8], tracker: &AtomicU64) {
        match self {
            Self::Jsonl(observer) => observer.observe(bytes, tracker),
            Self::Websocket(observer) => observer.observe(bytes, tracker),
        }
    }
}

#[derive(Debug, Default)]
struct JsonlSequenceObserverV2 {
    line: Vec<u8>,
}

impl JsonlSequenceObserverV2 {
    fn observe(&mut self, bytes: &[u8], tracker: &AtomicU64) {
        for &byte in bytes {
            if byte == b'\n' {
                observe_remora_link_sequence(&self.line, tracker);
                self.line.clear();
            } else if self.line.len() < MAX_OBSERVED_FRAME_BYTES {
                self.line.push(byte);
            } else {
                self.line.clear();
            }
        }
    }
}

#[derive(Debug, Default)]
struct WebsocketSequenceObserverV2 {
    upgrade_bytes: Vec<u8>,
    upgrade_complete: bool,
    bytes: Vec<u8>,
    fragmented_text: Vec<u8>,
    fragmented: bool,
}

impl WebsocketSequenceObserverV2 {
    fn observe(&mut self, bytes: &[u8], tracker: &AtomicU64) {
        if !self.upgrade_complete {
            if self.upgrade_bytes.len().saturating_add(bytes.len()) > MAX_OBSERVED_FRAME_BYTES {
                self.upgrade_bytes.clear();
                return;
            }
            self.upgrade_bytes.extend_from_slice(bytes);
            let Some(header_end) = self
                .upgrade_bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
            else {
                return;
            };
            let frame_bytes = self.upgrade_bytes.split_off(header_end);
            self.upgrade_bytes.clear();
            self.upgrade_complete = true;
            self.observe_frames(&frame_bytes, tracker);
            return;
        }
        self.observe_frames(bytes, tracker);
    }

    fn observe_frames(&mut self, bytes: &[u8], tracker: &AtomicU64) {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_OBSERVED_FRAME_BYTES {
            self.bytes.clear();
            self.fragmented_text.clear();
            self.fragmented = false;
            return;
        }
        self.bytes.extend_from_slice(bytes);
        loop {
            let Some((header_len, payload_len, opcode, final_frame)) = websocket_frame(&self.bytes)
            else {
                return;
            };
            if self.bytes.len() < header_len.saturating_add(payload_len) {
                return;
            }
            let payload = self.bytes[header_len..header_len + payload_len].to_vec();
            self.bytes.drain(..header_len + payload_len);
            match opcode {
                0x1 if final_frame => observe_remora_link_sequence(&payload, tracker),
                0x1 => {
                    self.fragmented_text.clear();
                    self.fragmented_text.extend_from_slice(&payload);
                    self.fragmented = true;
                }
                0x0 if self.fragmented => {
                    if self.fragmented_text.len().saturating_add(payload.len())
                        > MAX_OBSERVED_FRAME_BYTES
                    {
                        self.fragmented_text.clear();
                        self.fragmented = false;
                    } else {
                        self.fragmented_text.extend_from_slice(&payload);
                        if final_frame {
                            observe_remora_link_sequence(&self.fragmented_text, tracker);
                            self.fragmented_text.clear();
                            self.fragmented = false;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn websocket_frame(bytes: &[u8]) -> Option<(usize, usize, u8, bool)> {
    if bytes.len() < 2 {
        return None;
    }
    let final_frame = bytes[0] & 0x80 != 0;
    let opcode = bytes[0] & 0x0f;
    let masked = bytes[1] & 0x80 != 0;
    // Runtime output is server-to-client and therefore must not be masked.
    // Ignore malformed masked input; the real WebSocket decoder will reject it.
    if masked {
        return Some((bytes.len(), 0, 0xff, true));
    }
    let (header_len, payload_len) = match bytes[1] & 0x7f {
        value @ 0..=125 => (2, usize::from(value)),
        126 => {
            if bytes.len() < 4 {
                return None;
            }
            (4, usize::from(u16::from_be_bytes([bytes[2], bytes[3]])))
        }
        127 => {
            if bytes.len() < 10 {
                return None;
            }
            let length = u64::from_be_bytes(bytes[2..10].try_into().ok()?);
            (10, usize::try_from(length).ok()?)
        }
        _ => unreachable!(),
    };
    if payload_len > MAX_OBSERVED_FRAME_BYTES {
        return Some((bytes.len(), 0, 0xff, true));
    }
    Some((header_len, payload_len, opcode, final_frame))
}

fn observe_remora_link_sequence(payload: &[u8], tracker: &AtomicU64) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return;
    };
    let Some(sequence) = value
        .get(REMORA_LINK_SEQUENCE_FIELD)
        .and_then(|value| value.as_u64())
    else {
        return;
    };
    tracker.fetch_max(sequence, Ordering::Relaxed);
}

/// A transparent runtime byte stream that observes the sequence high-water
/// mark without treating socket reads as replay authority. Bytes and framing
/// are passed through unchanged to the upstream app-server client; reconnect
/// remains conservative until an application-level acknowledgement seam can
/// prove that a decoded event reached canonical state.
pub(crate) struct RemoraLinkRuntimeStreamV2<S> {
    inner: S,
    tracker: Arc<AtomicU64>,
    observer: SequenceObserverV2,
}

impl<S> RemoraLinkRuntimeStreamV2<S> {
    pub(crate) fn new(inner: S, wire: AgentWireV2, tracker: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            tracker,
            observer: SequenceObserverV2::new(wire),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for RemoraLinkRuntimeStreamV2<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buffer) {
            Poll::Ready(Ok(())) => {
                let after = buffer.filled().len();
                if after > before {
                    this.observer
                        .observe(&buffer.filled()[before..after], &this.tracker);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for RemoraLinkRuntimeStreamV2<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(crate) async fn connect_runtime_client_v2<S>(
    stream: S,
    wire: AgentWireV2,
    tracker: Arc<AtomicU64>,
    args: RemoteAppServerConnectArgs,
) -> Result<AppServerClient, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let stream = RemoraLinkRuntimeStreamV2::new(stream, wire, tracker);
    let label = "remora-link-v2://runtime".to_string();
    let remote = match runtime_framing(wire) {
        RuntimeFramingV2::Websocket => {
            RemoteAppServerClient::connect_websocket_stream(stream, args, label).await
        }
        RuntimeFramingV2::Jsonl => {
            codex_slingshot::json_line_wire::connect_json_line_stream(stream, args, label).await
        }
    }
    .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?;
    Ok(AppServerClient::Remote(remote))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct DropValue(Arc<AtomicUsize>);

    impl Drop for DropValue {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn registry_is_take_once_and_cross_wire_safe() {
        let now = Instant::now();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut registry = RetainedAttachmentRegistryV2::new(4, Duration::from_secs(30));
        registry
            .insert(
                "attach-a".into(),
                "host-a".into(),
                "codex".into(),
                DropValue(Arc::clone(&drops)),
                now,
            )
            .unwrap();

        assert!(matches!(
            registry.take("attach-a", "host-b", "codex", now),
            Err(AttachmentCustodyErrorV2::IdentityMismatch)
        ));
        assert_eq!(registry.len(), 1, "a mismatch must not consume custody");
        let value = registry.take("attach-a", "host-a", "codex", now).unwrap();
        assert_eq!(registry.len(), 0);
        assert!(matches!(
            registry.take("attach-a", "host-a", "codex", now),
            Err(AttachmentCustodyErrorV2::Unavailable)
        ));
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shell_attachment_custody_rejects_mismatch_and_is_take_once() {
        let now = Instant::now();
        let mut registry = RetainedAttachmentRegistryV2::new(2, Duration::from_secs(30));
        registry
            .insert(
                "shell-attachment".into(),
                "remora-link:host".into(),
                "shell".into(),
                7_u8,
                now,
            )
            .unwrap();

        assert!(matches!(
            registry.take("shell-attachment", "remora-link:other", "shell", now),
            Err(AttachmentCustodyErrorV2::IdentityMismatch)
        ));
        assert!(matches!(
            registry.take("shell-attachment", "remora-link:host", "codex", now),
            Err(AttachmentCustodyErrorV2::IdentityMismatch)
        ));
        assert_eq!(
            registry
                .take("shell-attachment", "remora-link:host", "shell", now)
                .unwrap(),
            7
        );
        assert!(matches!(
            registry.take("shell-attachment", "remora-link:host", "shell", now),
            Err(AttachmentCustodyErrorV2::Unavailable)
        ));
    }

    #[test]
    fn registry_expires_and_evicts_oldest_at_bound() {
        let now = Instant::now();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut registry = RetainedAttachmentRegistryV2::new(2, Duration::from_secs(5));
        for (id, offset) in [("a", 0), ("b", 1), ("c", 2)] {
            registry
                .insert(
                    id.into(),
                    "host".into(),
                    "codex".into(),
                    DropValue(Arc::clone(&drops)),
                    now + Duration::from_secs(offset),
                )
                .unwrap();
        }
        assert_eq!(registry.len(), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1, "oldest was evicted");
        assert!(matches!(
            registry.take("b", "host", "codex", now + Duration::from_secs(7)),
            Err(AttachmentCustodyErrorV2::Unavailable)
        ));
        assert_eq!(registry.len(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 3, "expired custody closes");
    }

    #[test]
    fn registry_keeps_concurrent_same_runtime_attachments_distinct() {
        let now = Instant::now();
        let mut registry = RetainedAttachmentRegistryV2::new(4, Duration::from_secs(30));
        registry
            .insert("first".into(), "host".into(), "codex".into(), 1, now)
            .unwrap();
        registry
            .insert("second".into(), "host".into(), "codex".into(), 2, now)
            .unwrap();
        assert_eq!(registry.take("second", "host", "codex", now), Ok(2));
        assert_eq!(registry.take("first", "host", "codex", now), Ok(1));
    }

    #[test]
    fn racing_takers_transfer_custody_exactly_once() {
        let now = Instant::now();
        let registry = Arc::new(std::sync::Mutex::new(RetainedAttachmentRegistryV2::new(
            4,
            Duration::from_secs(30),
        )));
        registry
            .lock()
            .unwrap()
            .insert("attachment".into(), "host".into(), "codex".into(), 7, now)
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let registry = Arc::clone(&registry);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .lock()
                        .unwrap()
                        .take("attachment", "host", "codex", now)
                })
            })
            .collect();
        barrier.wait();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Err(AttachmentCustodyErrorV2::Unavailable)))
                .count(),
            1
        );
    }

    #[test]
    fn jsonl_observer_tracks_only_remora_link_sequence() {
        let tracker = AtomicU64::new(0);
        let mut observer = JsonlSequenceObserverV2::default();
        observer.observe(
            br#"{"_unrelated_seq":99}
{"_remora_link_seq":4"#,
            &tracker,
        );
        observer.observe(
            br#"2}
{"_remora_link_seq":7}
"#,
            &tracker,
        );
        assert_eq!(tracker.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn websocket_observer_ignores_chunked_upgrade_then_tracks_frames_and_fragmentation() {
        let tracker = AtomicU64::new(0);
        let mut observer = WebsocketSequenceObserverV2::default();
        let first = br#"{"_remora_link_"#;
        let second = br#"seq":73}"#;
        let mut bytes = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        bytes.extend_from_slice(&[0x01, first.len() as u8]);
        bytes.extend_from_slice(first);
        bytes.extend_from_slice(&[0x80, second.len() as u8]);
        bytes.extend_from_slice(second);
        for chunk in bytes.chunks(3) {
            observer.observe(chunk, &tracker);
        }
        assert_eq!(tracker.load(Ordering::Relaxed), 73);
    }

    #[test]
    fn observer_tracks_only_the_current_sequence_field() {
        let tracker = AtomicU64::new(0);
        observe_remora_link_sequence(br#"{"_remora_link_seq":52,"other_seq":99}"#, &tracker);
        assert_eq!(tracker.load(Ordering::Relaxed), 52);
    }

    #[test]
    fn diagnostic_high_water_advances_only_from_payloads_delivered_to_the_stream() {
        let tracker = AtomicU64::new(0);
        assert_eq!(tracker.load(Ordering::Relaxed), 0);

        observe_remora_link_sequence(br#"{"_remora_link_seq":17}"#, &tracker);

        assert_eq!(tracker.load(Ordering::Relaxed), 17);
    }

    #[test]
    fn advertised_wire_selects_the_exact_upstream_framing() {
        assert_eq!(
            runtime_framing(AgentWireV2::Websocket),
            RuntimeFramingV2::Websocket
        );
        assert_eq!(runtime_framing(AgentWireV2::Jsonl), RuntimeFramingV2::Jsonl);
    }
}
