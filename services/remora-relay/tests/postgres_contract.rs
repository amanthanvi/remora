use std::{collections::BTreeSet, env, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use remora_relay::{
    CreateInstallationRequest, DeliveryOutcome, EventClass, IngestEventRequest, OpaqueId,
    PostgresRelayStore, PresentedCapability, PushEnvironment, PushProviderKind, RelayError,
    RelayMetrics, StoreLimits, TokenCipher,
};
use sqlx::PgPool;
use tokio::task::JoinSet;

fn event(index: usize, event_class: EventClass) -> IngestEventRequest {
    IngestEventRequest {
        event_id: OpaqueId::parse(format!("evt_postgres_{index:08}")).unwrap(),
        event_class,
        expires_at_ms: 120_000,
        ciphertext: URL_SAFE_NO_PAD.encode([index as u8; 32]),
        snapshot: None,
    }
}

fn capability(value: &remora_relay::IssuedCapability) -> PresentedCapability {
    PresentedCapability::parse(value.as_str().to_owned()).unwrap()
}

fn creation(key: &str) -> CreateInstallationRequest {
    CreateInstallationRequest::new(key).unwrap()
}

async fn connect_store(database_url: &str) -> Arc<PostgresRelayStore> {
    Arc::new(
        PostgresRelayStore::connect(
            database_url,
            32,
            TokenCipher::from_key([41; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .await
        .unwrap(),
    )
}

/// Exercises the production adapter against a real PostgreSQL server.
///
/// The test is intentionally environment-gated so ordinary unit-test runs do
/// not silently substitute SQLite for production behavior. CI and local
/// release verification set `REMORA_RELAY_TEST_DATABASE_URL` to an isolated
/// database that this test may truncate.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn postgres_cursor_outbox_and_token_generation_contract() {
    let Ok(database_url) = env::var("REMORA_RELAY_TEST_DATABASE_URL") else {
        #[cfg(feature = "required-postgres-tests")]
        panic!("REMORA_RELAY_TEST_DATABASE_URL is required for production-store verification");
        #[cfg(not(feature = "required-postgres-tests"))]
        {
            eprintln!("skipped: REMORA_RELAY_TEST_DATABASE_URL is not set");
            return;
        }
    };

    let store = connect_store(&database_url).await;
    assert!(store.ready().await);

    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::raw_sql("TRUNCATE TABLE installation_receipts, installations CASCADE")
        .execute(&pool)
        .await
        .unwrap();

    // Exercise the supported v3-to-v4 upgrade shape rather than only proving
    // that an empty database can be initialized. Version 4 adds durable
    // acknowledgements and encrypted installation-creation receipts.
    let legacy_installation = store
        .create_installation(&creation("txn_postgres_legacy_000000000000001"), 500)
        .await
        .unwrap();
    store
        .register_device(
            &legacy_installation.installation_id,
            &capability(&legacy_installation.manage_capability),
            PushProviderKind::Fcm,
            PushEnvironment::Production,
            "fcm-postgres-legacy-token",
            501,
        )
        .await
        .unwrap();
    let legacy_event = event(900, EventClass::StateChanged);
    store
        .ingest_event(
            &legacy_installation.installation_id,
            &capability(&legacy_installation.write_capability),
            &legacy_event,
            502,
        )
        .await
        .unwrap();
    let legacy_tombstoned = store
        .create_installation(&creation("txn_postgres_legacy_tombstone_0000001"), 503)
        .await
        .unwrap();
    store
        .tombstone_installation(
            &legacy_tombstoned.installation_id,
            &capability(&legacy_tombstoned.manage_capability),
            504,
        )
        .await
        .unwrap();
    drop(store);

    sqlx::raw_sql(
        "UPDATE relay_schema SET version = 3, updated_at_ms = 3;
         DROP TABLE installation_receipts;
         ALTER TABLE installations
             DROP CONSTRAINT installations_ack_below_high_watermark;
         ALTER TABLE installations DROP COLUMN acknowledged_through;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let store = connect_store(&database_url).await;
    let migrated_version =
        sqlx::query_scalar::<_, i32>("SELECT version FROM relay_schema WHERE singleton = TRUE")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(migrated_version, 4);
    let backfilled_receipts = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM event_receipts
         WHERE installation_id = $1 AND event_id = $2",
    )
    .bind(legacy_installation.installation_id.as_str())
    .bind(legacy_event.event_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(backfilled_receipts, 1);
    let has_lease_id = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'push_outbox'
               AND column_name = 'lease_id'
         )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(has_lease_id);
    let legacy_ack = sqlx::query_scalar::<_, i64>(
        "SELECT acknowledged_through FROM installations WHERE id = $1",
    )
    .bind(legacy_installation.installation_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(legacy_ack, 0);
    let legacy_tombstoned_ack = sqlx::query_scalar::<_, i64>(
        "SELECT acknowledged_through FROM installations WHERE id = $1",
    )
    .bind(legacy_tombstoned.installation_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(legacy_tombstoned_ack, 0);
    let has_installation_receipts = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.tables
             WHERE table_schema = current_schema()
               AND table_name = 'installation_receipts'
         )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(has_installation_receipts);

    // A current-schema restart must not rerun backfills or rewrite the schema
    // marker. A newer schema must fail closed instead of attempting rollback.
    sqlx::query("UPDATE relay_schema SET updated_at_ms = 12345 WHERE singleton = TRUE")
        .execute(&pool)
        .await
        .unwrap();
    drop(store);
    let store = connect_store(&database_url).await;
    let unchanged_marker = sqlx::query_scalar::<_, i64>(
        "SELECT updated_at_ms FROM relay_schema WHERE singleton = TRUE",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged_marker, 12_345);
    drop(store);

    sqlx::query("UPDATE relay_schema SET version = 99 WHERE singleton = TRUE")
        .execute(&pool)
        .await
        .unwrap();
    let newer_schema_error = PostgresRelayStore::connect(
        &database_url,
        2,
        TokenCipher::from_key([41; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .await
    .unwrap_err();
    assert!(matches!(newer_schema_error, RelayError::Configuration(_)));
    sqlx::query("UPDATE relay_schema SET version = 4 WHERE singleton = TRUE")
        .execute(&pool)
        .await
        .unwrap();

    let store = connect_store(&database_url).await;
    sqlx::raw_sql("TRUNCATE TABLE installation_receipts, installations CASCADE")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::raw_sql(
        "CREATE OR REPLACE FUNCTION remora_test_fail_creation()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             RAISE EXCEPTION 'injected creation failure';
         END;
         $$;
         CREATE TRIGGER remora_test_fail_creation
         AFTER INSERT ON installation_receipts
         FOR EACH ROW EXECUTE FUNCTION remora_test_fail_creation();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let fault_request = creation("txn_postgres_create_fault_00000000001");
    assert!(matches!(
        store.create_installation(&fault_request, 850).await,
        Err(RelayError::Postgres(_))
    ));
    sqlx::raw_sql(
        "DROP TRIGGER remora_test_fail_creation ON installation_receipts;
         DROP FUNCTION remora_test_fail_creation();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let fault_counts = sqlx::query_as::<_, (i64, i64)>(
        "SELECT (SELECT COUNT(*) FROM installations),
                (SELECT COUNT(*) FROM installation_receipts)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fault_counts, (0, 0));

    let concurrent_request = creation("txn_postgres_create_race_000000000001");
    let mut creates = JoinSet::new();
    for _ in 0..32 {
        let store = Arc::clone(&store);
        let request = concurrent_request.clone();
        creates.spawn(async move { store.create_installation(&request, 900).await.unwrap() });
    }
    let mut created = Vec::new();
    while let Some(result) = creates.join_next().await {
        created.push(result.unwrap());
    }
    let first_created = &created[0];
    for replayed in &created[1..] {
        assert_eq!(replayed.installation_id, first_created.installation_id);
        assert_eq!(
            replayed.write_capability.as_str(),
            first_created.write_capability.as_str()
        );
        assert_eq!(
            replayed.read_capability.as_str(),
            first_created.read_capability.as_str()
        );
        assert_eq!(
            replayed.manage_capability.as_str(),
            first_created.manage_capability.as_str()
        );
    }
    let create_counts = sqlx::query_as::<_, (i64, i64)>(
        "SELECT (SELECT COUNT(*) FROM installations),
                (SELECT COUNT(*) FROM installation_receipts)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(create_counts, (1, 1));
    let mut conflicting_create = concurrent_request.clone();
    conflicting_create.schema_version += 1;
    assert!(matches!(
        store.create_installation(&conflicting_create, 901).await,
        Err(RelayError::Conflict)
    ));
    let receipt_ciphertext =
        sqlx::query_scalar::<_, Vec<u8>>("SELECT response_ciphertext FROM installation_receipts")
            .fetch_one(&pool)
            .await
            .unwrap();
    for capability in [
        first_created.write_capability.as_str(),
        first_created.read_capability.as_str(),
        first_created.manage_capability.as_str(),
    ] {
        assert!(
            !receipt_ciphertext
                .windows(capability.len())
                .any(|window| window == capability.as_bytes())
        );
    }
    sqlx::query(
        "UPDATE installation_receipts
         SET response_ciphertext = set_byte(
             response_ciphertext, 0, (get_byte(response_ciphertext, 0) # 1)
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        store.create_installation(&concurrent_request, 902).await,
        Err(RelayError::Crypto)
    ));
    let post_tamper_installations =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM installations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(post_tamper_installations, 1);
    sqlx::raw_sql("TRUNCATE TABLE installation_receipts, installations CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    let expiring_request = creation("txn_postgres_receipt_expiry_0000000001");
    store
        .create_installation(&expiring_request, 800)
        .await
        .unwrap();
    sqlx::query("UPDATE installation_receipts SET response_expires_at_ms = 899")
        .execute(&pool)
        .await
        .unwrap();
    let receipt_maintenance = store.maintenance(900).await.unwrap();
    assert_eq!(receipt_maintenance.expired_installation_receipts, 1);
    assert!(matches!(
        store.create_installation(&expiring_request, 901).await,
        Err(RelayError::Tombstoned)
    ));
    let consumed_receipt = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), COUNT(response_ciphertext) FROM installation_receipts",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(consumed_receipt, (1, 0));
    sqlx::raw_sql("TRUNCATE TABLE installation_receipts, installations CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    drop(pool);

    let installation = store
        .create_installation(&creation("txn_postgres_main_0000000000000001"), 1_000)
        .await
        .unwrap();
    let write = capability(&installation.write_capability);
    let read = capability(&installation.read_capability);
    let manage = capability(&installation.manage_capability);

    let mut ingests = JoinSet::new();
    for index in 0..64 {
        let store = Arc::clone(&store);
        let installation_id = installation.installation_id.clone();
        let write = write.clone();
        ingests.spawn(async move {
            let cursor = store
                .ingest_event(
                    &installation_id,
                    &write,
                    &event(index, EventClass::StateChanged),
                    1_000,
                )
                .await
                .unwrap()
                .cursor;
            (index, cursor)
        });
    }
    let mut cursors = BTreeSet::new();
    let mut first_event_cursor = None;
    while let Some(result) = ingests.join_next().await {
        let (index, cursor) = result.unwrap();
        cursors.insert(cursor);
        if index == 0 {
            first_event_cursor = Some(cursor);
        }
    }
    assert_eq!(cursors, (1..=64).collect());

    let replay = store
        .ingest_event(
            &installation.installation_id,
            &write,
            &event(0, EventClass::StateChanged),
            1_001,
        )
        .await
        .unwrap();
    assert_eq!(replay.cursor, first_event_cursor.unwrap());
    assert!(replay.replayed);
    let page = store
        .fetch_events(&installation.installation_id, &read, 0, 100, 1_001)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 64);
    assert_eq!(page.high_watermark, 64);
    assert!(!page.reset_required);

    let mut acknowledgements = JoinSet::new();
    for through_cursor in (1..=64).rev() {
        let store = Arc::clone(&store);
        let installation_id = installation.installation_id.clone();
        let read = read.clone();
        acknowledgements.spawn(async move {
            store
                .acknowledge(&installation_id, &read, through_cursor, 1_002)
                .await
                .unwrap()
        });
    }
    while let Some(result) = acknowledgements.join_next().await {
        result.unwrap();
    }
    let final_ack = store
        .acknowledge(&installation.installation_id, &read, 1, 1_002)
        .await
        .unwrap();
    assert_eq!(final_ack.acknowledged_through, 64);
    assert!(final_ack.replayed);
    assert!(matches!(
        store
            .acknowledge(&installation.installation_id, &write, 64, 1_002)
            .await,
        Err(RelayError::Unauthorized)
    ));
    assert!(matches!(
        store
            .acknowledge(&installation.installation_id, &read, 65, 1_002)
            .await,
        Err(RelayError::Invalid(_))
    ));
    let reopened = connect_store(&database_url).await;
    let durable_ack = reopened
        .acknowledge(&installation.installation_id, &read, 32, 1_002)
        .await
        .unwrap();
    assert_eq!(durable_ack.acknowledged_through, 64);
    assert!(durable_ack.replayed);

    let race_installation = store
        .create_installation(&creation("txn_postgres_ack_races_00000000001"), 1_100)
        .await
        .unwrap();
    let race_read = capability(&race_installation.read_capability);
    let race_write = capability(&race_installation.write_capability);
    let race_manage = capability(&race_installation.manage_capability);
    let ingest_race = {
        let store = Arc::clone(&store);
        let installation_id = race_installation.installation_id.clone();
        let write = race_write.clone();
        tokio::spawn(async move {
            store
                .ingest_event(
                    &installation_id,
                    &write,
                    &event(700, EventClass::ConnectionChanged),
                    1_101,
                )
                .await
        })
    };
    let ack_race = {
        let store = Arc::clone(&store);
        let installation_id = race_installation.installation_id.clone();
        let read = race_read.clone();
        tokio::spawn(async move { store.acknowledge(&installation_id, &read, 1, 1_101).await })
    };
    ingest_race.await.unwrap().unwrap();
    match ack_race.await.unwrap() {
        Ok(response) => assert_eq!(response.acknowledged_through, 1),
        Err(RelayError::Invalid(_)) => {}
        other => panic!("unexpected ingest/ack race result: {other:?}"),
    }
    store
        .acknowledge(&race_installation.installation_id, &race_read, 1, 1_102)
        .await
        .unwrap();

    let tombstone_race = {
        let store = Arc::clone(&store);
        let installation_id = race_installation.installation_id.clone();
        let manage = race_manage.clone();
        tokio::spawn(async move {
            store
                .tombstone_installation(&installation_id, &manage, 1_103)
                .await
        })
    };
    let final_ack_race = {
        let store = Arc::clone(&store);
        let installation_id = race_installation.installation_id.clone();
        let read = race_read.clone();
        tokio::spawn(async move { store.acknowledge(&installation_id, &read, 1, 1_103).await })
    };
    tombstone_race.await.unwrap().unwrap();
    match final_ack_race.await.unwrap() {
        Ok(response) => assert_eq!(response.acknowledged_through, 1),
        Err(RelayError::Tombstoned) => {}
        other => panic!("unexpected tombstone/ack race result: {other:?}"),
    }
    let invariant_pool = PgPool::connect(&database_url).await.unwrap();
    let (next_sequence, acknowledged_through, tombstoned_at_ms) =
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT next_sequence, acknowledged_through, tombstoned_at_ms
             FROM installations WHERE id = $1",
        )
        .bind(race_installation.installation_id.as_str())
        .fetch_one(&invariant_pool)
        .await
        .unwrap();
    assert!(acknowledged_through >= 0 && acknowledged_through < next_sequence);
    assert!(tombstoned_at_ms.is_some());
    drop(invariant_pool);

    let registration = store
        .register_device(
            &installation.installation_id,
            &manage,
            PushProviderKind::Fcm,
            PushEnvironment::Production,
            "fcm-postgres-generation-one-token",
            1_002,
        )
        .await
        .unwrap();
    assert_eq!(registration.generation, 1);

    for (index, class) in [
        EventClass::StateChanged,
        EventClass::ActivityChanged,
        EventClass::ConnectionChanged,
        EventClass::SecurityChanged,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event(100 + index, class),
                1_003,
            )
            .await
            .unwrap();
    }

    let left = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.lease_deliveries(1_004, 2).await.unwrap() })
    };
    let right = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.lease_deliveries(1_004, 2).await.unwrap() })
    };
    let mut leases = left.await.unwrap();
    leases.extend(right.await.unwrap());
    assert_eq!(leases.len(), 4);
    assert_eq!(
        leases
            .iter()
            .map(|lease| lease.outbox_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    assert!(
        leases
            .iter()
            .all(|lease| lease.registration_generation == 1)
    );

    let stale_lease = leases.remove(0);
    let rotated = store
        .register_device(
            &installation.installation_id,
            &manage,
            PushProviderKind::Fcm,
            PushEnvironment::Production,
            "fcm-postgres-generation-two-token",
            1_005,
        )
        .await
        .unwrap();
    assert_eq!(rotated.registration_id, registration.registration_id);
    assert_eq!(rotated.generation, 2);
    assert!(rotated.replaced);

    store
        .complete_delivery(
            &stale_lease.outbox_id,
            stale_lease.generation,
            stale_lease.registration_generation,
            &stale_lease.lease_id,
            DeliveryOutcome::InvalidToken,
            1_006,
        )
        .await
        .unwrap();
    assert_eq!(store.diagnostics().await.unwrap().active_registrations, 1);
    let mut current_leases = store.lease_deliveries(1_007, 10).await.unwrap();
    assert_eq!(current_leases.len(), 4);
    assert!(
        current_leases
            .iter()
            .all(|lease| lease.registration_generation == 2)
    );

    let expiring_lease = current_leases.remove(0);
    for lease in current_leases {
        store
            .complete_delivery(
                &lease.outbox_id,
                lease.generation,
                lease.registration_generation,
                &lease.lease_id,
                DeliveryOutcome::Accepted,
                1_008,
            )
            .await
            .unwrap();
    }
    let recovered_lease = store.lease_deliveries(31_008, 1).await.unwrap().remove(0);
    assert_eq!(recovered_lease.outbox_id, expiring_lease.outbox_id);
    assert_ne!(recovered_lease.lease_id, expiring_lease.lease_id);
    store
        .complete_delivery(
            &expiring_lease.outbox_id,
            expiring_lease.generation,
            expiring_lease.registration_generation,
            &expiring_lease.lease_id,
            DeliveryOutcome::Accepted,
            31_009,
        )
        .await
        .unwrap();
    assert_eq!(store.diagnostics().await.unwrap().leased_outbox, 1);
    store
        .complete_delivery(
            &recovered_lease.outbox_id,
            recovered_lease.generation,
            recovered_lease.registration_generation,
            &recovered_lease.lease_id,
            DeliveryOutcome::Accepted,
            31_010,
        )
        .await
        .unwrap();

    store
        .ingest_event(
            &installation.installation_id,
            &write,
            &event(200, EventClass::StateChanged),
            32_000,
        )
        .await
        .unwrap();
    let invalid_lease = store.lease_deliveries(32_001, 1).await.unwrap().remove(0);
    let rotate = {
        let store = Arc::clone(&store);
        let installation_id = installation.installation_id.clone();
        let manage = manage.clone();
        tokio::spawn(async move {
            store
                .register_device(
                    &installation_id,
                    &manage,
                    PushProviderKind::Fcm,
                    PushEnvironment::Production,
                    "fcm-postgres-generation-three-token",
                    32_002,
                )
                .await
        })
    };
    let invalidate = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .complete_delivery(
                    &invalid_lease.outbox_id,
                    invalid_lease.generation,
                    invalid_lease.registration_generation,
                    &invalid_lease.lease_id,
                    DeliveryOutcome::InvalidToken,
                    32_002,
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        rotate.await.unwrap().unwrap();
        invalidate.await.unwrap().unwrap();
    })
    .await
    .expect("rotation and invalid-token completion must not deadlock");
    assert_eq!(store.diagnostics().await.unwrap().active_registrations, 1);

    let expiring_installation = store
        .create_installation(&creation("txn_postgres_expiring_0000000000001"), 40_000)
        .await
        .unwrap();
    store
        .ingest_event(
            &expiring_installation.installation_id,
            &capability(&expiring_installation.write_capability),
            &event(300, EventClass::StateChanged),
            40_000,
        )
        .await
        .unwrap();
    let independent_installation = store
        .create_installation(&creation("txn_postgres_independent_00000000001"), 40_000)
        .await
        .unwrap();
    let mut held_lock = PgPool::connect(&database_url)
        .await
        .unwrap()
        .begin()
        .await
        .unwrap();
    sqlx::query("SELECT id FROM installations WHERE id = $1 FOR UPDATE")
        .bind(expiring_installation.installation_id.as_str())
        .execute(&mut *held_lock)
        .await
        .unwrap();
    let maintenance = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.maintenance(130_000).await })
    };
    let independent_ingest = {
        let store = Arc::clone(&store);
        let installation_id = independent_installation.installation_id.clone();
        let write = capability(&independent_installation.write_capability);
        tokio::spawn(async move {
            store
                .ingest_event(
                    &installation_id,
                    &write,
                    &event(301, EventClass::StateChanged),
                    40_001,
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        maintenance.await.unwrap().unwrap();
        independent_ingest.await.unwrap().unwrap();
    })
    .await
    .expect("maintenance must skip a locked unrelated installation");
    held_lock.rollback().await.unwrap();

    let expired_replay = store
        .ingest_event(
            &installation.installation_id,
            &write,
            &event(0, EventClass::StateChanged),
            130_001,
        )
        .await
        .unwrap();
    assert_eq!(expired_replay.cursor, first_event_cursor.unwrap());
    assert!(expired_replay.replayed);
    assert!(
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event(0, EventClass::SecurityChanged),
                130_002,
            )
            .await
            .is_err()
    );
}
