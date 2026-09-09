use super::*;
use remora_bridge_core::session::SessionRegistryConfig;
use serde_json::json;

async fn wire_pair(websocket: bool) -> (Wire, Wire) {
    let (client, server) = tokio::io::duplex(4096);
    let (client, server) = tokio::join!(
        Wire::open(client, websocket, true),
        Wire::open(server, websocket, false)
    );
    (client.unwrap(), server.unwrap())
}

#[tokio::test]
async fn detached_completion_replays_and_initialize_is_not_forwarded_twice() {
    detached_completion(false).await;
    detached_completion(true).await;
}

async fn detached_completion(websocket: bool) {
    let registry = SessionRegistry::new(SessionRegistryConfig {
        ring_max_msgs: 1,
        ..SessionRegistryConfig::default()
    });
    let session = registry.get_or_create("device".to_owned(), "codex");
    let (host_backend, mut runtime) = wire_pair(websocket).await;
    let (tx, rx) = mpsc::channel(32);
    let backend_task = tokio::spawn(serve_backend(
        host_backend,
        rx,
        Arc::clone(&session),
        Arc::downgrade(&registry),
    ));
    let (mut phone, host) = wire_pair(websocket).await;
    let attachment = tokio::spawn(serve_attachment(
        host,
        Arc::clone(&session),
        None,
        tx.clone(),
    ));
    phone.send(&json!({"id": 1, "method": "initialize", "params": {"capabilities": {"experimentalApi": true}}})).await.unwrap();
    let initialize = runtime.read().await.unwrap().unwrap();
    assert_eq!(initialize["method"], "initialize");
    assert_ne!(initialize["id"], 1);
    runtime
        .send(&json!({"id": initialize["id"], "result": {"userAgent": "fixture"}}))
        .await
        .unwrap();
    assert_eq!(phone.read().await.unwrap().unwrap()["id"], 1);
    phone.send(&json!({"method": "initialized"})).await.unwrap();
    assert_eq!(
        runtime.read().await.unwrap().unwrap()["method"],
        "initialized"
    );
    runtime.send(&json!({"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"command":"fixture"}})).await.unwrap();
    assert_eq!(phone.read().await.unwrap().unwrap()["id"], "approval-1");
    drop(phone);
    let _ = tokio::time::timeout(Duration::from_secs(2), attachment)
        .await
        .unwrap()
        .unwrap();
    let before = session.state_barrier();
    runtime
        .send(&json!({"method": "turn/completed", "params": {"marker": "detached-completion"}}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.state_barrier() == before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (mut phone, host) = wire_pair(websocket).await;
    let attachment = tokio::spawn(serve_attachment(
        host,
        Arc::clone(&session),
        Some(session.peek_seq().0 - 1),
        tx.clone(),
    ));
    let completion = phone.read().await.unwrap().unwrap();
    assert_eq!(completion["params"]["marker"], "detached-completion");
    phone.send(&json!({"id": 1, "method": "initialize", "params": {"capabilities": {"experimentalApi": true}}})).await.unwrap();
    let initialized = phone.read().await.unwrap().unwrap();
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["userAgent"], "fixture");
    assert_eq!(phone.read().await.unwrap().unwrap()["id"], "approval-1");
    phone
        .send(&json!({"id":"approval-1","result":{"decision":"decline"}}))
        .await
        .unwrap();
    assert_eq!(runtime.read().await.unwrap().unwrap()["id"], "approval-1");
    phone.send(&json!({"id": 2, "method": "initialize", "params": {"capabilities": {"experimentalApi": false}}})).await.unwrap();
    assert_eq!(
        phone.read().await.unwrap().unwrap()["error"]["code"],
        -32602
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), runtime.read())
            .await
            .is_err()
    );
    drop(phone);
    let _ = attachment.await.unwrap();
    registry.release("device", "codex");
    tokio::time::timeout(Duration::from_secs(2), backend_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn shutdown_joins_retained_tasks_and_releases_only_their_session() {
    let registry = SessionRegistry::new(SessionRegistryConfig::default());
    let session = registry.get_or_create("device".to_owned(), "codex");
    let guard = BackendGuard {
        session: Arc::clone(&session),
        registry: Arc::downgrade(&registry),
    };
    let task = tokio::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let (sender, _receiver) = mpsc::channel(1);
    let sessions = CodexSessions::default();
    sessions.sessions.lock().await.insert(
        "device".to_owned(),
        Retained {
            session,
            sender,
            task,
        },
    );
    sessions.shutdown().await;
    assert!(registry.get("device", "codex").is_none());
    assert!(sessions.sessions.lock().await.is_empty());
}

#[tokio::test]
async fn jsonl_rejects_truncation_malformed_and_oversized_frames() {
    for bytes in [
        b"{".to_vec(),
        b"not-json\n".to_vec(),
        vec![b'x'; MAX_FRAME + 1],
    ] {
        let (mut client, server) = tokio::io::duplex(4096);
        let writer = tokio::spawn(async move {
            let _ = client.write_all(&bytes).await;
        });
        let mut wire = Wire::open(server, false, false).await.unwrap();
        assert!(wire.read().await.is_err());
        drop(wire);
        writer.await.unwrap();
    }
}

#[tokio::test]
async fn old_attachment_cannot_detach_replacement_or_deliver_old_responses() {
    let registry = SessionRegistry::new(SessionRegistryConfig::default());
    let session = registry.get_or_create("device".to_owned(), "codex");
    let (host, runtime) = tokio::io::duplex(4096);
    let (tx, rx) = mpsc::channel(32);
    let backend = tokio::spawn(serve_backend(
        Wire::open(host, false, true).await.unwrap(),
        rx,
        Arc::clone(&session),
        Arc::downgrade(&registry),
    ));
    let mut runtime = Wire::open(runtime, false, false).await.unwrap();
    let first = session.install_attachment(None);
    tx.send(Command {
        generation: first.generation,
        value: json!({"id": 7, "method": "thread/read", "params": {}}),
    })
    .await
    .unwrap();
    let old = runtime.read().await.unwrap().unwrap();
    let mut second = session.install_attachment(None);
    session.drop_attachment_generation(first.generation);
    runtime
        .send(&json!({"id": old["id"], "result": {"old": true}}))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), second.live_rx.recv())
            .await
            .is_err()
    );
    runtime
        .send(&json!({"method": "turn/completed", "params": {}}))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), second.live_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .payload["method"],
        "turn/completed"
    );
    registry.release("device", "codex");
    backend.await.unwrap().unwrap();
}
