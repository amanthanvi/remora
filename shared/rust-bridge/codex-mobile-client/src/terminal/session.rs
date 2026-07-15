use super::backend::{TerminalBackend, TerminalBackendEvent, open_backend, validate_size};
use super::ssh::TerminalSshAuth;
use super::ssh_known_hosts::TerminalSshTrustStore;
use crate::ffi::shared::shared_runtime;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

const OUTPUT_CHANNEL_CAPACITY: usize = 1024;
const SESSION_REPLAY_LIMIT_BYTES: usize = 64 * 1024;
const SESSION_REPLAY_LIMIT_EVENTS: usize = 4096;
const MAX_OUTPUT_EVENT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalBackendKind {
    RemoteAlleycat {
        node_id: String,
        token: String,
        relay: Option<String>,
        shell: Option<String>,
    },
    RemoteSsh {
        host: String,
        port: u16,
        username: String,
        auth: TerminalSshAuth,
        shell: Option<String>,
        accept_unknown_host: bool,
        cwd: Option<String>,
    },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TerminalError {
    #[error("Unsupported: {detail}")]
    Unsupported { detail: String },
    #[error("Invalid size: {detail}")]
    InvalidSize { detail: String },
    #[error("Backend: {detail}")]
    Backend { detail: String },
    #[error("Terminal session is closed")]
    Closed,
}

#[uniffi::export(callback_interface)]
pub trait TerminalOutputListener: Send + Sync {
    fn on_bytes(&self, data: Vec<u8>);
    fn on_exit(&self, code: i32);
}

/// Cursor-bearing terminal stream used by new renderers. Unlike the legacy
/// byte callback, this surface makes replay truncation and live gaps explicit.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalOutputSnapshot {
    pub bytes: Vec<u8>,
    pub base_sequence: u64,
    pub latest_sequence: Option<u64>,
    pub truncated: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalOutputStreamEvent {
    Snapshot { snapshot: TerminalOutputSnapshot },
    Output { sequence: u64, data: Vec<u8> },
    Exited { sequence: u64, code: i32 },
    Reset { snapshot: TerminalOutputSnapshot },
}

#[uniffi::export(callback_interface)]
pub trait TerminalOutputEventListener: Send + Sync {
    fn on_event(&self, event: TerminalOutputStreamEvent);
}

/// Lifetime handle for one cursor-bearing output subscription.
///
/// Dropping or explicitly cancelling the handle releases the callback even
/// while the terminal session itself remains alive. This lets native views
/// reattach without accumulating duplicate listeners.
#[derive(uniffi::Object)]
pub struct TerminalOutputSubscription {
    cancel_tx: watch::Sender<bool>,
}

#[uniffi::export]
impl TerminalOutputSubscription {
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }
}

impl Drop for TerminalOutputSubscription {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(true);
    }
}

#[derive(Debug, Clone)]
enum TerminalOutputEvent {
    Bytes(Vec<u8>),
    Exit(i32),
}

#[derive(Debug, Clone)]
struct TerminalOutputEnvelope {
    sequence: u64,
    event: TerminalOutputEvent,
}

#[derive(Debug, Default)]
struct TerminalOutputHistory {
    next_sequence: u64,
    events: VecDeque<TerminalOutputEnvelope>,
    byte_count: usize,
    truncated: bool,
}

#[derive(Debug)]
enum TerminalOutputRecovery {
    Replay(Vec<TerminalOutputEnvelope>),
    Reset(TerminalOutputSnapshot),
}

impl TerminalOutputHistory {
    fn record(&mut self, event: &TerminalOutputEvent) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);

        let stored_event = match event {
            TerminalOutputEvent::Bytes(data) if data.len() > SESSION_REPLAY_LIMIT_BYTES => {
                self.truncated = true;
                TerminalOutputEvent::Bytes(data[data.len() - SESSION_REPLAY_LIMIT_BYTES..].to_vec())
            }
            TerminalOutputEvent::Bytes(data) => TerminalOutputEvent::Bytes(data.clone()),
            TerminalOutputEvent::Exit(code) => TerminalOutputEvent::Exit(*code),
        };
        if let TerminalOutputEvent::Bytes(data) = &stored_event {
            self.byte_count = self.byte_count.saturating_add(data.len());
        }
        self.events.push_back(TerminalOutputEnvelope {
            sequence,
            event: stored_event,
        });
        self.trim();
        sequence
    }

    fn snapshot(&self) -> TerminalOutputSnapshot {
        let mut bytes = Vec::with_capacity(self.byte_count);
        let mut exit_code = None;
        for envelope in &self.events {
            match &envelope.event {
                TerminalOutputEvent::Bytes(data) => bytes.extend_from_slice(data),
                TerminalOutputEvent::Exit(code) => exit_code = Some(*code),
            }
        }
        TerminalOutputSnapshot {
            bytes,
            base_sequence: self
                .events
                .front()
                .map(|event| event.sequence)
                .unwrap_or(self.next_sequence),
            latest_sequence: self.next_sequence.checked_sub(1),
            truncated: self.truncated,
            exit_code,
        }
    }

    fn recover_from(&self, expected_sequence: u64) -> TerminalOutputRecovery {
        if expected_sequence >= self.next_sequence {
            return TerminalOutputRecovery::Replay(Vec::new());
        }

        let first_retained = self
            .events
            .front()
            .map(|event| event.sequence)
            .unwrap_or(self.next_sequence);
        if expected_sequence < first_retained {
            return TerminalOutputRecovery::Reset(self.snapshot());
        }

        TerminalOutputRecovery::Replay(
            self.events
                .iter()
                .filter(|event| event.sequence >= expected_sequence)
                .cloned()
                .collect(),
        )
    }

    fn trim(&mut self) {
        while self.byte_count > SESSION_REPLAY_LIMIT_BYTES
            || self.events.len() > SESSION_REPLAY_LIMIT_EVENTS
        {
            let Some(removed) = self.events.pop_front() else {
                self.byte_count = 0;
                break;
            };
            self.truncated = true;
            if let TerminalOutputEvent::Bytes(data) = removed.event {
                self.byte_count = self.byte_count.saturating_sub(data.len());
            }
        }
    }
}

#[derive(uniffi::Object)]
pub struct TerminalSession {
    backend: Arc<dyn TerminalBackend>,
    output_tx: broadcast::Sender<TerminalOutputEnvelope>,
    output_history: Arc<Mutex<TerminalOutputHistory>>,
    retained_output_subscriptions: Mutex<Vec<Arc<TerminalOutputSubscription>>>,
    closed: Arc<AtomicBool>,
    rt: Arc<tokio::runtime::Runtime>,
}

#[uniffi::export(async_runtime = "tokio")]
impl TerminalSession {
    #[uniffi::constructor]
    pub async fn open(
        backend: TerminalBackendKind,
        size: TerminalSize,
    ) -> Result<Self, TerminalError> {
        let rt = shared_runtime();
        let (backend, output_rx) = open_backend(backend, size, None).await?;
        Ok(Self::from_open_backend(backend, output_rx, rt))
    }

    /// Same as [`Self::open`] but consults `trust_store` for the SSH backend
    /// host-key pin policy. Non-SSH backends ignore `trust_store`.
    #[uniffi::constructor]
    pub async fn open_with_trust_store(
        backend: TerminalBackendKind,
        size: TerminalSize,
        trust_store: Arc<TerminalSshTrustStore>,
    ) -> Result<Self, TerminalError> {
        let rt = shared_runtime();
        let (backend, output_rx) = open_backend(backend, size, Some(trust_store)).await?;
        Ok(Self::from_open_backend(backend, output_rx, rt))
    }

    pub async fn write_input(&self, data: Vec<u8>) -> Result<(), TerminalError> {
        self.ensure_open()?;
        self.backend.write(&data).await
    }

    pub async fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
        self.ensure_open()?;
        validate_size(size)?;
        self.backend.resize(size).await
    }

    pub fn subscribe_output(&self, listener: Box<dyn TerminalOutputListener>) {
        self.subscribe_output_events_retained(Box::new(LegacyTerminalOutputAdapter { listener }));
    }

    /// Subscribe to a cursor-bearing snapshot-plus-tail stream. Subscription
    /// happens before the history snapshot is read, closing the attach race.
    /// A lagged receiver is repaired from retained history or receives an
    /// explicit `Reset` snapshot when the missing range has been truncated.
    pub fn subscribe_output_events(
        &self,
        listener: Box<dyn TerminalOutputEventListener>,
    ) -> Arc<TerminalOutputSubscription> {
        self.start_output_event_subscription(listener)
    }

    pub fn output_snapshot(&self) -> TerminalOutputSnapshot {
        self.output_history.lock().unwrap().snapshot()
    }

    pub async fn close_session(&self) -> Result<(), TerminalError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.backend.close().await
    }
}

impl TerminalSession {
    pub(crate) fn subscribe_output_events_retained(
        &self,
        listener: Box<dyn TerminalOutputEventListener>,
    ) {
        let subscription = self.start_output_event_subscription(listener);
        self.retained_output_subscriptions
            .lock()
            .unwrap()
            .push(subscription);
    }

    fn start_output_event_subscription(
        &self,
        listener: Box<dyn TerminalOutputEventListener>,
    ) -> Arc<TerminalOutputSubscription> {
        let rx = self.output_tx.subscribe();
        let snapshot = self.output_history.lock().unwrap().snapshot();
        let listener: Arc<dyn TerminalOutputEventListener> = Arc::from(listener);
        let history = Arc::clone(&self.output_history);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let subscription = Arc::new(TerminalOutputSubscription { cancel_tx });
        self.rt.spawn(stream_terminal_output(
            rx,
            snapshot,
            listener,
            history,
            Some(cancel_rx),
        ));
        subscription
    }

    fn from_open_backend(
        backend: Arc<dyn TerminalBackend>,
        mut output_rx: tokio::sync::mpsc::Receiver<TerminalBackendEvent>,
        rt: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        let (output_tx, _) = broadcast::channel(OUTPUT_CHANNEL_CAPACITY);
        let output_history = Arc::new(Mutex::new(TerminalOutputHistory::default()));
        let forward_tx = output_tx.clone();
        let forward_history = output_history.clone();
        rt.spawn(async move {
            while let Some(event) = output_rx.recv().await {
                match event {
                    TerminalBackendEvent::Bytes(data) => {
                        // Bound individual broadcast envelopes. History remains
                        // byte-bounded and a slow subscriber can explicitly
                        // reset if this burst exceeds its channel capacity.
                        for chunk in data.chunks(MAX_OUTPUT_EVENT_BYTES) {
                            let output = TerminalOutputEvent::Bytes(chunk.to_vec());
                            let sequence = forward_history.lock().unwrap().record(&output);
                            let _ = forward_tx.send(TerminalOutputEnvelope {
                                sequence,
                                event: output,
                            });
                        }
                    }
                    TerminalBackendEvent::Exit(code) => {
                        let output = TerminalOutputEvent::Exit(code);
                        let sequence = forward_history.lock().unwrap().record(&output);
                        let _ = forward_tx.send(TerminalOutputEnvelope {
                            sequence,
                            event: output,
                        });
                        break;
                    }
                }
            }
        });
        Self {
            backend,
            output_tx,
            output_history,
            retained_output_subscriptions: Mutex::new(Vec::new()),
            closed: Arc::new(AtomicBool::new(false)),
            rt,
        }
    }

    fn ensure_open(&self) -> Result<(), TerminalError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(TerminalError::Closed)
        } else {
            Ok(())
        }
    }
}

async fn stream_terminal_output(
    mut rx: broadcast::Receiver<TerminalOutputEnvelope>,
    snapshot: TerminalOutputSnapshot,
    listener: Arc<dyn TerminalOutputEventListener>,
    history: Arc<Mutex<TerminalOutputHistory>>,
    mut cancel_rx: Option<watch::Receiver<bool>>,
) {
    let mut expected_sequence = snapshot
        .latest_sequence
        .map(|sequence| sequence.saturating_add(1))
        .unwrap_or(snapshot.base_sequence);
    let already_exited = snapshot.exit_code.is_some();
    listener.on_event(TerminalOutputStreamEvent::Snapshot { snapshot });
    if already_exited {
        return;
    }

    loop {
        let next = tokio::select! {
            _ = wait_for_terminal_output_cancellation(&mut cancel_rx) => break,
            next = rx.recv() => next,
        };
        match next {
            Ok(envelope) if envelope.sequence < expected_sequence => {}
            Ok(envelope) => {
                if envelope.sequence > expected_sequence
                    && recover_terminal_output(&listener, &history, &mut expected_sequence)
                {
                    break;
                }
                if envelope.sequence < expected_sequence {
                    continue;
                }
                if deliver_terminal_output_event(&listener, envelope.clone()) {
                    break;
                }
                expected_sequence = envelope.sequence.saturating_add(1);
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if recover_terminal_output(&listener, &history, &mut expected_sequence) {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn wait_for_terminal_output_cancellation(cancel_rx: &mut Option<watch::Receiver<bool>>) {
    let Some(cancel_rx) = cancel_rx else {
        std::future::pending::<()>().await;
        return;
    };
    if *cancel_rx.borrow() {
        return;
    }
    loop {
        if cancel_rx.changed().await.is_err() || *cancel_rx.borrow() {
            return;
        }
    }
}

fn recover_terminal_output(
    listener: &Arc<dyn TerminalOutputEventListener>,
    history: &Arc<Mutex<TerminalOutputHistory>>,
    expected_sequence: &mut u64,
) -> bool {
    let recovery = history.lock().unwrap().recover_from(*expected_sequence);
    match recovery {
        TerminalOutputRecovery::Replay(events) => {
            for envelope in events {
                if envelope.sequence < *expected_sequence {
                    continue;
                }
                let sequence = envelope.sequence;
                if deliver_terminal_output_event(listener, envelope) {
                    *expected_sequence = sequence.saturating_add(1);
                    return true;
                }
                *expected_sequence = sequence.saturating_add(1);
            }
            false
        }
        TerminalOutputRecovery::Reset(snapshot) => {
            *expected_sequence = snapshot
                .latest_sequence
                .map(|sequence| sequence.saturating_add(1))
                .unwrap_or(snapshot.base_sequence);
            let exited = snapshot.exit_code.is_some();
            listener.on_event(TerminalOutputStreamEvent::Reset { snapshot });
            exited
        }
    }
}

fn deliver_terminal_output_event(
    listener: &Arc<dyn TerminalOutputEventListener>,
    envelope: TerminalOutputEnvelope,
) -> bool {
    match envelope.event {
        TerminalOutputEvent::Bytes(data) => {
            listener.on_event(TerminalOutputStreamEvent::Output {
                sequence: envelope.sequence,
                data,
            });
            false
        }
        TerminalOutputEvent::Exit(code) => {
            listener.on_event(TerminalOutputStreamEvent::Exited {
                sequence: envelope.sequence,
                code,
            });
            true
        }
    }
}

struct LegacyTerminalOutputAdapter {
    listener: Box<dyn TerminalOutputListener>,
}

impl TerminalOutputEventListener for LegacyTerminalOutputAdapter {
    fn on_event(&self, event: TerminalOutputStreamEvent) {
        match event {
            TerminalOutputStreamEvent::Snapshot { snapshot }
            | TerminalOutputStreamEvent::Reset { snapshot } => {
                if !snapshot.bytes.is_empty() {
                    self.listener.on_bytes(snapshot.bytes);
                }
                if let Some(code) = snapshot.exit_code {
                    self.listener.on_exit(code);
                }
            }
            TerminalOutputStreamEvent::Output { data, .. } => self.listener.on_bytes(data),
            TerminalOutputStreamEvent::Exited { code, .. } => self.listener.on_exit(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    #[derive(Default)]
    struct FakeBackend {
        writes: Mutex<Vec<Vec<u8>>>,
        resizes: Mutex<Vec<TerminalSize>>,
        closed: AtomicBool,
    }

    #[async_trait]
    impl TerminalBackend for FakeBackend {
        async fn write(&self, data: &[u8]) -> Result<(), TerminalError> {
            self.writes.lock().unwrap().push(data.to_vec());
            Ok(())
        }

        async fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
            self.resizes.lock().unwrap().push(size);
            Ok(())
        }

        async fn close(&self) -> Result<(), TerminalError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct CapturingListener {
        bytes: Arc<Mutex<Vec<Vec<u8>>>>,
        exits: Arc<Mutex<Vec<i32>>>,
    }

    #[derive(Default)]
    struct CapturingEventListener {
        events: Mutex<Vec<TerminalOutputStreamEvent>>,
    }

    impl TerminalOutputEventListener for CapturingEventListener {
        fn on_event(&self, event: TerminalOutputStreamEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    struct SharedCapturingEventListener(Arc<CapturingEventListener>);

    impl TerminalOutputEventListener for SharedCapturingEventListener {
        fn on_event(&self, event: TerminalOutputStreamEvent) {
            self.0.on_event(event);
        }
    }

    impl TerminalOutputListener for CapturingListener {
        fn on_bytes(&self, data: Vec<u8>) {
            self.bytes.lock().unwrap().push(data);
        }

        fn on_exit(&self, code: i32) {
            self.exits.lock().unwrap().push(code);
        }
    }

    async fn wait_for_latest_sequence(session: &TerminalSession, expected: u64) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if session.output_snapshot().latest_sequence == Some(expected) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal output did not reach expected sequence");
    }

    async fn wait_for_condition(mut condition: impl FnMut() -> bool, message: &'static str) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if condition() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect(message);
    }

    #[tokio::test]
    async fn session_forwards_io_and_lifecycle_to_backend() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, rx) = mpsc::channel(8);
        let rt = shared_runtime();
        let session = TerminalSession::from_open_backend(backend.clone(), rx, rt);

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let exits = Arc::new(Mutex::new(Vec::new()));
        session.subscribe_output(Box::new(CapturingListener {
            bytes: bytes.clone(),
            exits: exits.clone(),
        }));

        session.write_input(b"echo hi\n".to_vec()).await.unwrap();
        session
            .resize(TerminalSize {
                cols: 100,
                rows: 40,
            })
            .await
            .unwrap();

        tx.send(TerminalBackendEvent::Bytes(b"hi\n".to_vec()))
            .await
            .unwrap();
        tx.send(TerminalBackendEvent::Exit(0)).await.unwrap();
        wait_for_condition(
            || !bytes.lock().unwrap().is_empty() && !exits.lock().unwrap().is_empty(),
            "legacy terminal listener did not receive output and exit",
        )
        .await;

        assert_eq!(
            backend.writes.lock().unwrap().as_slice(),
            &[b"echo hi\n".to_vec()]
        );
        assert_eq!(
            backend.resizes.lock().unwrap().as_slice(),
            &[TerminalSize {
                cols: 100,
                rows: 40
            }]
        );
        assert_eq!(bytes.lock().unwrap().as_slice(), &[b"hi\n".to_vec()]);
        assert_eq!(exits.lock().unwrap().as_slice(), &[0]);

        session.close_session().await.unwrap();
        assert!(backend.closed.load(Ordering::SeqCst));
        assert!(matches!(
            session.write_input(Vec::new()).await,
            Err(TerminalError::Closed)
        ));
    }

    #[tokio::test]
    async fn session_replays_output_emitted_before_listener_subscribes() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, rx) = mpsc::channel(8);
        let rt = shared_runtime();
        let session = TerminalSession::from_open_backend(backend, rx, rt);

        tx.send(TerminalBackendEvent::Bytes(b"early\n".to_vec()))
            .await
            .unwrap();
        tx.send(TerminalBackendEvent::Exit(7)).await.unwrap();
        wait_for_latest_sequence(&session, 1).await;

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let exits = Arc::new(Mutex::new(Vec::new()));
        session.subscribe_output(Box::new(CapturingListener {
            bytes: bytes.clone(),
            exits: exits.clone(),
        }));
        wait_for_condition(
            || !bytes.lock().unwrap().is_empty() && !exits.lock().unwrap().is_empty(),
            "late terminal listener did not receive snapshot and exit",
        )
        .await;

        assert_eq!(bytes.lock().unwrap().as_slice(), &[b"early\n".to_vec()]);
        assert_eq!(exits.lock().unwrap().as_slice(), &[7]);
    }

    #[tokio::test]
    async fn cancelling_subscription_stops_future_output_callbacks() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, rx) = mpsc::channel(8);
        let session = TerminalSession::from_open_backend(backend, rx, shared_runtime());
        let listener = Arc::new(CapturingEventListener::default());
        let subscription = session.subscribe_output_events(Box::new(SharedCapturingEventListener(
            Arc::clone(&listener),
        )));

        wait_for_condition(
            || !listener.events.lock().unwrap().is_empty(),
            "subscriber did not receive its initial snapshot",
        )
        .await;
        subscription.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        tx.send(TerminalBackendEvent::Bytes(b"ignored".to_vec()))
            .await
            .unwrap();
        wait_for_latest_sequence(&session, 0).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            listener.events.lock().unwrap().as_slice(),
            [TerminalOutputStreamEvent::Snapshot { .. }]
        ));
    }

    #[test]
    fn history_repairs_retained_sequences_exactly_once() {
        let mut history = TerminalOutputHistory::default();
        for value in 0_u8..8 {
            history.record(&TerminalOutputEvent::Bytes(vec![value]));
        }

        let TerminalOutputRecovery::Replay(events) = history.recover_from(3) else {
            panic!("retained range should replay without reset");
        };
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7]
        );
        assert_eq!(
            events
                .into_iter()
                .map(|event| match event.event {
                    TerminalOutputEvent::Bytes(data) => data[0],
                    TerminalOutputEvent::Exit(_) => panic!("unexpected exit"),
                })
                .collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7]
        );
    }

    #[tokio::test]
    async fn lagged_subscriber_replays_every_retained_sequence_exactly_once() {
        let backend = Arc::new(FakeBackend::default());
        let event_count = OUTPUT_CHANNEL_CAPACITY + 64;
        let (tx, rx) = mpsc::channel(event_count + 1);
        let session = TerminalSession::from_open_backend(backend, rx, shared_runtime());
        let output_rx = session.output_tx.subscribe();
        let snapshot = session.output_snapshot();

        for value in 0..event_count {
            tx.send(TerminalBackendEvent::Bytes(vec![(value % 251) as u8]))
                .await
                .unwrap();
        }
        tx.send(TerminalBackendEvent::Exit(0)).await.unwrap();
        wait_for_latest_sequence(&session, event_count as u64).await;

        let listener = Arc::new(CapturingEventListener::default());
        let listener_dyn: Arc<dyn TerminalOutputEventListener> = listener.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream_terminal_output(
                output_rx,
                snapshot,
                listener_dyn,
                Arc::clone(&session.output_history),
                None,
            ),
        )
        .await
        .expect("lag recovery did not reach the exit event");

        let events = listener.events.lock().unwrap();
        assert!(matches!(
            events.first(),
            Some(TerminalOutputStreamEvent::Snapshot { snapshot })
                if snapshot.latest_sequence.is_none()
        ));
        assert_eq!(events.len(), event_count + 2);
        assert_eq!(
            events[1..=event_count]
                .iter()
                .map(|event| match event {
                    TerminalOutputStreamEvent::Output { sequence, .. } => *sequence,
                    other => panic!("unexpected replay event: {other:?}"),
                })
                .collect::<Vec<_>>(),
            (0..event_count as u64).collect::<Vec<_>>()
        );
        assert!(matches!(
            events.last(),
            Some(TerminalOutputStreamEvent::Exited {
                sequence,
                code: 0
            }) if *sequence == event_count as u64
        ));
    }

    #[tokio::test]
    async fn lagged_subscriber_receives_reset_when_retained_event_limit_is_exceeded() {
        let backend = Arc::new(FakeBackend::default());
        let event_count = SESSION_REPLAY_LIMIT_EVENTS + OUTPUT_CHANNEL_CAPACITY;
        let (tx, rx) = mpsc::channel(event_count + 1);
        let session = TerminalSession::from_open_backend(backend, rx, shared_runtime());
        let output_rx = session.output_tx.subscribe();
        let snapshot = session.output_snapshot();

        for value in 0..event_count {
            tx.send(TerminalBackendEvent::Bytes(vec![(value % 251) as u8]))
                .await
                .unwrap();
        }
        tx.send(TerminalBackendEvent::Exit(29)).await.unwrap();
        wait_for_latest_sequence(&session, event_count as u64).await;

        let listener = Arc::new(CapturingEventListener::default());
        let listener_dyn: Arc<dyn TerminalOutputEventListener> = listener.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream_terminal_output(
                output_rx,
                snapshot,
                listener_dyn,
                Arc::clone(&session.output_history),
                None,
            ),
        )
        .await
        .expect("lag reset did not terminate at retained exit");

        let events = listener.events.lock().unwrap();
        let Some(TerminalOutputStreamEvent::Reset { snapshot }) = events.get(1) else {
            panic!("lag beyond retained history must reset: {events:?}");
        };
        assert_eq!(events.len(), 2);
        assert!(snapshot.truncated);
        assert_eq!(
            snapshot.base_sequence,
            (event_count + 1 - SESSION_REPLAY_LIMIT_EVENTS) as u64
        );
        assert_eq!(snapshot.latest_sequence, Some(event_count as u64));
        assert_eq!(snapshot.bytes.len(), SESSION_REPLAY_LIMIT_EVENTS - 1);
        assert_eq!(snapshot.exit_code, Some(29));
    }

    #[tokio::test]
    async fn attach_during_output_burst_deduplicates_snapshot_tail_and_orders_exit_last() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, rx) = mpsc::channel(32);
        let session = TerminalSession::from_open_backend(backend, rx, shared_runtime());

        for value in 0_u8..4 {
            tx.send(TerminalBackendEvent::Bytes(vec![value]))
                .await
                .unwrap();
        }
        wait_for_latest_sequence(&session, 3).await;

        let output_rx = session.output_tx.subscribe();
        let snapshot = session.output_snapshot();
        for value in 4_u8..7 {
            tx.send(TerminalBackendEvent::Bytes(vec![value]))
                .await
                .unwrap();
        }
        tx.send(TerminalBackendEvent::Exit(17)).await.unwrap();
        wait_for_latest_sequence(&session, 7).await;

        let listener = Arc::new(CapturingEventListener::default());
        let listener_dyn: Arc<dyn TerminalOutputEventListener> = listener.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream_terminal_output(
                output_rx,
                snapshot,
                listener_dyn,
                Arc::clone(&session.output_history),
                None,
            ),
        )
        .await
        .expect("snapshot tail did not reach exit");

        let events = listener.events.lock().unwrap();
        assert_eq!(events.len(), 5);
        let TerminalOutputStreamEvent::Snapshot { snapshot } = &events[0] else {
            panic!("first event must be a snapshot");
        };
        assert_eq!(snapshot.bytes, vec![0, 1, 2, 3]);
        assert_eq!(snapshot.latest_sequence, Some(3));
        assert_eq!(
            events[1..4]
                .iter()
                .map(|event| match event {
                    TerminalOutputStreamEvent::Output { sequence, data } => (*sequence, data[0]),
                    other => panic!("unexpected tail event: {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![(4, 4), (5, 5), (6, 6)]
        );
        assert!(matches!(
            events.last(),
            Some(TerminalOutputStreamEvent::Exited {
                sequence: 7,
                code: 17
            })
        ));
    }

    #[tokio::test]
    async fn backend_output_is_split_into_bounded_stream_events() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, rx) = mpsc::channel(2);
        let session = TerminalSession::from_open_backend(backend, rx, shared_runtime());
        let output_rx = session.output_tx.subscribe();
        let snapshot = session.output_snapshot();

        let payload = (0..(MAX_OUTPUT_EVENT_BYTES * 2 + 7))
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        tx.send(TerminalBackendEvent::Bytes(payload.clone()))
            .await
            .unwrap();
        tx.send(TerminalBackendEvent::Exit(0)).await.unwrap();
        wait_for_latest_sequence(&session, 3).await;

        let listener = Arc::new(CapturingEventListener::default());
        let listener_dyn: Arc<dyn TerminalOutputEventListener> = listener.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream_terminal_output(
                output_rx,
                snapshot,
                listener_dyn,
                Arc::clone(&session.output_history),
                None,
            ),
        )
        .await
        .expect("bounded output stream did not reach exit");

        let events = listener.events.lock().unwrap();
        let chunks = events[1..4]
            .iter()
            .map(|event| match event {
                TerminalOutputStreamEvent::Output { data, .. } => data.as_slice(),
                other => panic!("unexpected bounded output event: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            chunks.iter().map(|chunk| chunk.len()).collect::<Vec<_>>(),
            vec![MAX_OUTPUT_EVENT_BYTES, MAX_OUTPUT_EVENT_BYTES, 7]
        );
        assert_eq!(
            chunks.into_iter().flatten().copied().collect::<Vec<_>>(),
            payload
        );
    }

    #[test]
    fn history_emits_explicit_reset_when_gap_is_no_longer_retained() {
        let mut history = TerminalOutputHistory::default();
        let chunk_size = 1024;
        let event_count = SESSION_REPLAY_LIMIT_BYTES / chunk_size + 1;
        for value in 0..event_count {
            history.record(&TerminalOutputEvent::Bytes(vec![
                (value % 251) as u8;
                chunk_size
            ]));
        }

        let TerminalOutputRecovery::Reset(snapshot) = history.recover_from(0) else {
            panic!("truncated missing range must reset");
        };
        assert!(snapshot.truncated);
        assert!(snapshot.base_sequence > 0);
        assert_eq!(snapshot.bytes.len(), SESSION_REPLAY_LIMIT_BYTES);
        assert_eq!(snapshot.latest_sequence, Some((event_count - 1) as u64));
        assert_eq!(snapshot.exit_code, None);
    }

    #[test]
    fn lag_recovery_delivers_reset_event_instead_of_silent_gap() {
        let history = Arc::new(Mutex::new(TerminalOutputHistory::default()));
        for value in 0..(SESSION_REPLAY_LIMIT_EVENTS + 1) {
            history
                .lock()
                .unwrap()
                .record(&TerminalOutputEvent::Bytes(vec![(value % 251) as u8]));
        }
        let listener = Arc::new(CapturingEventListener::default());
        let listener_dyn: Arc<dyn TerminalOutputEventListener> = listener.clone();
        let mut expected_sequence = 0;

        assert!(!recover_terminal_output(
            &listener_dyn,
            &history,
            &mut expected_sequence
        ));
        let events = listener.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let TerminalOutputStreamEvent::Reset { snapshot } = &events[0] else {
            panic!("missing range must produce a reset event");
        };
        assert!(snapshot.truncated);
        assert_eq!(
            expected_sequence,
            snapshot.latest_sequence.unwrap().saturating_add(1)
        );
    }

    #[test]
    fn snapshot_preserves_output_before_exit_and_reports_cursor() {
        let mut history = TerminalOutputHistory::default();
        history.record(&TerminalOutputEvent::Bytes(b"first".to_vec()));
        history.record(&TerminalOutputEvent::Bytes(b"second".to_vec()));
        history.record(&TerminalOutputEvent::Exit(9));

        assert_eq!(
            history.snapshot(),
            TerminalOutputSnapshot {
                bytes: b"firstsecond".to_vec(),
                base_sequence: 0,
                latest_sequence: Some(2),
                truncated: false,
                exit_code: Some(9),
            }
        );
    }

    #[test]
    fn rejects_zero_sized_terminal() {
        let error = validate_size(TerminalSize { cols: 0, rows: 24 }).unwrap_err();
        assert!(matches!(error, TerminalError::InvalidSize { .. }));
    }

    #[tokio::test]
    #[ignore = "requires a live alleycat daemon; set REMORA_TERMINAL_LIVE_ALLEYCAT_PAIR"]
    async fn live_remote_alleycat_terminal_round_trips_shell_io() {
        let pair_json = match std::env::var("REMORA_TERMINAL_LIVE_ALLEYCAT_PAIR") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!("skipping: REMORA_TERMINAL_LIVE_ALLEYCAT_PAIR is not set");
                return;
            }
        };
        let pair = crate::alleycat::parse_pair_payload(&pair_json).expect("parse pair payload");
        let session = TerminalSession::open(
            TerminalBackendKind::RemoteAlleycat {
                node_id: pair.node_id,
                token: pair.token,
                relay: pair.relay,
                shell: Some("/bin/sh".to_string()),
            },
            TerminalSize { cols: 77, rows: 31 },
        )
        .await
        .expect("open remote shell terminal");

        enum LiveEvent {
            Bytes(Vec<u8>),
            Exit(i32),
        }

        struct LiveListener {
            tx: tokio::sync::mpsc::UnboundedSender<LiveEvent>,
        }

        impl TerminalOutputListener for LiveListener {
            fn on_bytes(&self, data: Vec<u8>) {
                let _ = self.tx.send(LiveEvent::Bytes(data));
            }

            fn on_exit(&self, code: i32) {
                let _ = self.tx.send(LiveEvent::Exit(code));
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        session.subscribe_output(Box::new(LiveListener { tx }));
        session
            .write_input(b"printf 'remote-mobile-ready\n'; stty size; exit 0\n".to_vec())
            .await
            .expect("write shell input");

        let mut output = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let exit_code = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out waiting for remote shell output; got {:?}",
                    String::from_utf8_lossy(&output)
                );
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(LiveEvent::Bytes(bytes))) => output.extend(bytes),
                Ok(Some(LiveEvent::Exit(code))) => {
                    break code;
                }
                Ok(None) => panic!("remote terminal listener closed"),
                Err(_) => panic!(
                    "timed out waiting for remote shell output; got {:?}",
                    String::from_utf8_lossy(&output)
                ),
            }
            if String::from_utf8_lossy(&output).contains("remote-mobile-ready")
                && String::from_utf8_lossy(&output).contains("31 77")
            {
                // Keep waiting for the shell/exit notification so the test
                // proves the full lifecycle, not just stdout delivery.
            }
        };

        let output = String::from_utf8_lossy(&output);
        assert!(
            output.contains("remote-mobile-ready"),
            "expected command output, got {output:?}"
        );
        assert!(
            output.contains("31 77"),
            "expected stty size from remote PTY, got {output:?}"
        );
        assert_eq!(exit_code, 0);
        session.close_session().await.ok();
    }
}
