use super::*;
use crate::pairing_v2::DeviceScopeV2;
use remora_bridge_core::session::SessionRegistryConfig;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

fn auth() -> AuthorizationContextV2 {
    AuthorizationContextV2 {
        credential_id: "credential-device-0001".to_owned(),
        auth_epoch: 0,
        selected_runtime_ids: vec!["codex".to_owned()],
        granted_scopes: vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
        ],
    }
}

fn fixture(
    origin: String,
) -> (
    tempfile::TempDir,
    Arc<SessionRegistry>,
    Arc<BackgroundRelay>,
) {
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("bootstrap");
    write_private(&token, b"bootstrap-authority-for-tests-only-000000").unwrap();
    let registry = SessionRegistry::new(SessionRegistryConfig::default());
    let relay = BackgroundRelay::load(
        BackgroundRelayConfig {
            origin,
            bootstrap_token_file: token,
            allow_loopback_http: true,
        },
        directory.path().join("relay.json"),
        Arc::clone(&registry),
    )
    .unwrap();
    (directory, registry, relay)
}

fn issued() -> Value {
    json!({"schema_version": 1, "installation_id": "ins_installation_test0001", "write_capability": "w".repeat(43),
        "read_capability": "r".repeat(43), "manage_capability": "m".repeat(43)})
}

async fn http_fixture(
    responses: Vec<Option<Value>>,
) -> (
    String,
    mpsc::UnboundedReceiver<Value>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (send, receive) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                bytes.push(stream.read_u8().await.unwrap());
                assert!(bytes.len() < MAX_BODY);
            }
            let headers = String::from_utf8(bytes).unwrap();
            let size: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; size];
            stream.read_exact(&mut body).await.unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            send.send(request.clone()).unwrap();
            if let Some(mut response) = response {
                let status = response
                    .get("_status")
                    .and_then(Value::as_u64)
                    .unwrap_or(200);
                if response.get("_publish").is_some() {
                    response = json!({"schema_version":1,"event_id":request["event_id"],"cursor":1,"replayed":true});
                }
                let body = serde_json::to_vec(&response).unwrap();
                let header = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        }
    });
    (origin, receive, task)
}

#[tokio::test]
async fn ambiguous_publication_replays_exact_body_after_restart() {
    let (origin, mut received, server) =
        http_fixture(vec![Some(issued()), None, Some(json!({"_publish":true}))]).await;
    let (_directory, registry, relay) = fixture(origin);
    let auth = auth();
    let issued = relay.enroll(&auth, "endpoint", "command-1").await.unwrap();
    received.recv().await.unwrap();
    relay
        .commit(&auth, &issued.installation_id, "command-1")
        .await
        .unwrap();
    assert!(relay.publish_one(&auth).await.is_err());
    let first = received.recv().await.unwrap();
    assert!(
        relay
            .barrier(&auth, &issued.installation_id, 1)
            .await
            .is_err()
    );
    let restored =
        BackgroundRelay::load(relay.config.clone(), relay.path.clone(), registry).unwrap();
    restored.publish_one(&auth).await.unwrap();
    assert_eq!(first, received.recv().await.unwrap());
    assert!(
        restored
            .barrier(&auth, &issued.installation_id, 1)
            .await
            .is_ok()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn expired_rejected_wake_is_replaced_but_ambiguous_wake_is_retained() {
    let (origin, mut received, server) = http_fixture(vec![
        Some(issued()),
        None,
        Some(json!({"_status":400})),
        Some(json!({"_publish":true})),
    ])
    .await;
    let (_directory, _registry, relay) = fixture(origin);
    let auth = auth();
    let issued = relay.enroll(&auth, "endpoint", "command-1").await.unwrap();
    received.recv().await.unwrap();
    relay
        .commit(&auth, &issued.installation_id, "command-1")
        .await
        .unwrap();
    assert!(relay.publish_one(&auth).await.is_err());
    let first = received.recv().await.unwrap();
    {
        let mut journal = relay.journal.lock().await;
        let pending = journal
            .entries
            .get_mut(&auth.credential_id)
            .unwrap()
            .pending
            .as_mut()
            .unwrap();
        assert_eq!(pending.event_id, first["event_id"]);
        pending.expires_at_ms = now_ms().unwrap() - 1;
        relay.persist(&journal).unwrap();
    }
    assert!(relay.publish_one(&auth).await.is_err());
    received.recv().await.unwrap();
    assert!(
        relay.journal.lock().await.entries[&auth.credential_id]
            .pending
            .is_none()
    );
    relay.publish_one(&auth).await.unwrap();
    assert_ne!(
        first["event_id"],
        received.recv().await.unwrap()["event_id"]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn changed_origin_and_incomplete_custody_are_rejected_on_reload() {
    let (origin, mut received, server) = http_fixture(vec![Some(issued())]).await;
    let (_directory, registry, relay) = fixture(origin);
    relay
        .enroll(&auth(), "endpoint", "command-1")
        .await
        .unwrap();
    received.recv().await.unwrap();
    let mut config = relay.config.clone();
    config.origin = "https://different.example".to_owned();
    assert!(BackgroundRelay::load(config, relay.path.clone(), Arc::clone(&registry)).is_err());
    let mut state = relay.journal.lock().await.clone();
    state
        .entries
        .get_mut(&auth().credential_id)
        .unwrap()
        .write_capability = None;
    relay.persist(&state).unwrap();
    assert!(BackgroundRelay::load(relay.config.clone(), relay.path.clone(), registry).is_err());
    server.await.unwrap();
}

#[test]
fn origin_and_private_custody_fail_closed() {
    for origin in [
        "http://example.com",
        "http://localhost:9000",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?secret=x",
        "https://example.com/#fragment",
    ] {
        assert!(validate_origin(origin, true).is_err(), "{origin}");
    }
    assert!(validate_origin("http://127.0.0.1:9000", false).is_err());
    assert!(validate_origin("http://127.0.0.1:9000", true).is_ok());
    assert!(validate_origin("https://relay.example", false).is_ok());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    write_private(&path, b"private").unwrap();
    assert_eq!(&*read_private(&path, 10).unwrap(), b"private");
    assert!(read_private(&path, 2).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        symlink(&path, dir.path().join("link")).unwrap();
        assert!(read_private(&dir.path().join("link"), 10).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private(&path, 10).is_err());
    }
}

#[tokio::test]
async fn ambiguous_provision_replays_durable_key_and_commit_erases_transfer() {
    let (origin, mut received, server) = http_fixture(vec![None, Some(issued())]).await;
    let (_directory, registry, relay) = fixture(origin);
    let auth = auth();
    assert!(relay.enroll(&auth, "endpoint", "command-1").await.is_err());
    let first = received.recv().await.unwrap();
    let restored =
        BackgroundRelay::load(relay.config.clone(), relay.path.clone(), registry).unwrap();
    let result = restored
        .enroll(&auth, "endpoint", "command-1")
        .await
        .unwrap();
    let replay = received.recv().await.unwrap();
    assert_eq!(first, replay);
    assert!(
        first["idempotency_key"]
            .as_str()
            .unwrap()
            .starts_with("txn_")
    );
    assert!(!format!("{result:?}").contains(&"r".repeat(43)));
    assert_eq!(
        restored
            .enroll(&auth, "endpoint", "command-1")
            .await
            .unwrap()
            .installation_id,
        result.installation_id
    );
    assert!(
        restored
            .enroll(&auth, "wrong-endpoint", "command-1")
            .await
            .is_err()
    );
    assert!(
        restored
            .enroll(&auth, "endpoint", "different-command")
            .await
            .is_err()
    );
    assert!(
        restored
            .barrier(&auth, &result.installation_id, 1)
            .await
            .is_err()
    );
    let receipt = restored
        .commit(&auth, &result.installation_id, "command-1")
        .await
        .unwrap();
    assert_eq!(
        receipt,
        restored
            .commit(&auth, &result.installation_id, "command-1")
            .await
            .unwrap()
    );
    assert!(
        restored
            .enroll(&auth, "endpoint", "command-1")
            .await
            .is_err()
    );
    let stored = read_private(&restored.path, MAX_JOURNAL).unwrap();
    let stored = std::str::from_utf8(&stored).unwrap();
    assert!(!stored.contains(&"r".repeat(43)) && !stored.contains(&"m".repeat(43)));
    server.await.unwrap();
}

#[tokio::test]
async fn barrier_changes_on_state_events_session_replacement_and_host_restart_not_reads() {
    let (origin, mut received, server) = http_fixture(vec![Some(issued())]).await;
    let (_directory, registry, relay) = fixture(origin);
    let auth = auth();
    let issued = relay.enroll(&auth, "endpoint", "command-1").await.unwrap();
    received.recv().await.unwrap();
    let a = relay
        .barrier(&auth, &issued.installation_id, 0)
        .await
        .unwrap();
    let session = registry.get_or_create(auth.credential_id.clone(), "codex");
    let b = relay
        .barrier(&auth, &issued.installation_id, 0)
        .await
        .unwrap();
    assert_ne!(a.barrier_id, b.barrier_id);
    session.enqueue(json!({"id": 3, "result": {}}));
    assert_eq!(
        b.barrier_id,
        relay
            .barrier(&auth, &issued.installation_id, 0)
            .await
            .unwrap()
            .barrier_id
    );
    session.enqueue(json!({"method": "turn/completed", "params": {}}));
    let c = relay
        .barrier(&auth, &issued.installation_id, 0)
        .await
        .unwrap();
    assert_ne!(b.barrier_id, c.barrier_id);
    registry.release(&auth.credential_id, "codex");
    registry.get_or_create(auth.credential_id.clone(), "codex");
    assert_ne!(
        c.barrier_id,
        relay
            .barrier(&auth, &issued.installation_id, 0)
            .await
            .unwrap()
            .barrier_id
    );
    let restored =
        BackgroundRelay::load(relay.config.clone(), relay.path.clone(), registry).unwrap();
    assert_ne!(
        c.host_epoch,
        restored
            .barrier(&auth, &issued.installation_id, 0)
            .await
            .unwrap()
            .host_epoch
    );
    server.await.unwrap();
}

/// Run with REMORA_RELAY_TEST_BINARY pointing at the locally built service.
#[tokio::test]
#[ignore = "requires locally built remora-relay executable"]
async fn real_loopback_relay_publication_and_authenticated_cursor_barrier() {
    let binary = std::env::var_os("REMORA_RELAY_TEST_BINARY").expect("relay binary path");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (directory, registry, relay) = fixture(format!("http://{address}"));
    let config = directory.path().join("service.toml");
    write_private(&config, format!("deployment_profile = \"local_development\"\n[server]\nbind = \"{address}\"\n[database]\nkind = \"local_sqlite\"\npath = {:?}\n[security]\ntoken_key_path = {:?}\nallow_unauthenticated_bootstrap_on_loopback = true\n[push]\nmode = \"mock\"\n", directory.path().join("relay.sqlite"), directory.path().join("token.key")).as_bytes()).unwrap();
    let mut child = tokio::process::Command::new(binary)
        .arg("serve")
        .arg("--config")
        .arg(config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if relay
                .client
                .get(format!("http://{address}/health/ready"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            assert!(child.try_wait().unwrap().is_none(), "relay process exited");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let auth = auth();
    let issued = relay.enroll(&auth, "endpoint", "command-1").await.unwrap();
    let read_capability = issued.read_capability.clone();
    relay
        .commit(&auth, &issued.installation_id, "command-1")
        .await
        .unwrap();
    let session = registry.get_or_create(auth.credential_id.clone(), "codex");
    let attachment = session.install_attachment(None);
    session.drop_attachment_generation(attachment.generation);
    session.enqueue(json!({"method": "turn/completed", "params": {"secretTranscript": "must never leave host"}}));
    relay.publish_one(&auth).await.unwrap();
    let state = relay.journal.lock().await;
    let cursor = state.entries[&auth.credential_id].latest_cursor;
    drop(state);
    assert!(cursor > 0);
    let barrier = relay
        .barrier(&auth, &issued.installation_id, cursor)
        .await
        .unwrap();
    assert_eq!(barrier.runtime_states[0].state_revision, 1);
    assert!(
        relay
            .barrier(&auth, &issued.installation_id, cursor + 1)
            .await
            .is_err()
    );
    let events: Value = relay
        .json_request(
            relay
                .client
                .get(format!(
                    "http://{address}/v1/installations/{}/events?after=0",
                    issued.installation_id
                ))
                .bearer_auth(&read_capability.0),
        )
        .await
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    assert!(!events.to_string().contains("must never leave host"));
    relay.publish_one(&auth).await.unwrap();
    assert_eq!(
        relay.journal.lock().await.entries[&auth.credential_id].latest_cursor,
        cursor
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}
