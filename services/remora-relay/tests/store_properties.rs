use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Barrier},
    thread,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use proptest::prelude::*;
use remora_relay::{
    CreateInstallationRequest, EventClass, FaultInjector, FaultPoint, IngestEventRequest, OpaqueId,
    PresentedCapability, PushEnvironment, PushProviderKind, RelayError, RelayMetrics, RelayStore,
    SnapshotUpdate, StoreLimits, TokenCipher,
};
use rusqlite::Connection;

fn event(index: usize, expiry: i64) -> IngestEventRequest {
    IngestEventRequest {
        event_id: OpaqueId::parse(format!("evt_property_{index:08}")).unwrap(),
        event_class: EventClass::StateChanged,
        expires_at_ms: expiry,
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

#[test]
fn concurrent_ingest_allocates_gap_free_unique_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let store = RelayStore::open(
        directory.path().join("relay.sqlite3"),
        TokenCipher::from_key([11; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .unwrap();
    let installation = store
        .create_installation(&creation("txn_property_concurrent_00000000001"), 1_000)
        .unwrap();
    let write = capability(&installation.write_capability);
    let barrier = Arc::new(Barrier::new(48));
    let mut threads = Vec::new();
    for index in 0..48 {
        let store = store.clone();
        let installation_id = installation.installation_id.clone();
        let write = write.clone();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            store
                .ingest_event(&installation_id, &write, &event(index, 30_000), 1_000)
                .unwrap()
                .cursor
        }));
    }
    let cursors: BTreeSet<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(cursors, (1..=48).collect());
    assert_eq!(store.diagnostics().unwrap().retained_events, 48);
}

#[test]
fn concurrent_installation_retries_create_one_exact_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("relay.sqlite3");
    let store = RelayStore::open(
        &database,
        TokenCipher::from_key([16; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .unwrap();
    let request = creation("txn_property_create_race_000000000001");
    let barrier = Arc::new(Barrier::new(32));
    let mut threads = Vec::new();
    for _ in 0..32 {
        let store = store.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            store.create_installation(&request, 1_000).unwrap()
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let first = &results[0];
    for result in &results[1..] {
        assert_eq!(result.installation_id, first.installation_id);
        assert_eq!(
            result.write_capability.as_str(),
            first.write_capability.as_str()
        );
        assert_eq!(
            result.read_capability.as_str(),
            first.read_capability.as_str()
        );
        assert_eq!(
            result.manage_capability.as_str(),
            first.manage_capability.as_str()
        );
    }
    let connection = Connection::open(&database).unwrap();
    let installations: i64 = connection
        .query_row("SELECT COUNT(*) FROM installations", [], |row| row.get(0))
        .unwrap();
    let receipts: i64 = connection
        .query_row("SELECT COUNT(*) FROM installation_receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((installations, receipts), (1, 1));
}

#[test]
fn concurrent_acknowledgements_never_regress() {
    let directory = tempfile::tempdir().unwrap();
    let store = RelayStore::open(
        directory.path().join("relay.sqlite3"),
        TokenCipher::from_key([17; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .unwrap();
    let installation = store
        .create_installation(&creation("txn_property_ack_race_0000000000001"), 1_000)
        .unwrap();
    let write = capability(&installation.write_capability);
    let read = capability(&installation.read_capability);
    for index in 0..64 {
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event(index, 30_000),
                1_000,
            )
            .unwrap();
    }
    let barrier = Arc::new(Barrier::new(64));
    let mut threads = Vec::new();
    for through_cursor in (1..=64).rev() {
        let store = store.clone();
        let installation_id = installation.installation_id.clone();
        let read = read.clone();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            store
                .acknowledge(&installation_id, &read, through_cursor, 1_001)
                .unwrap()
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let final_ack = store
        .acknowledge(&installation.installation_id, &read, 1, 1_002)
        .unwrap();
    assert_eq!(final_ack.acknowledged_through, 64);
    assert!(final_ack.replayed);
}

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> remora_relay::Result<()> {
        if point == self.0 {
            Err(RelayError::InjectedFault)
        } else {
            Ok(())
        }
    }
}

#[test]
fn installation_and_receipt_failures_rollback_as_one_transaction() {
    for point in [
        FaultPoint::AfterInstallationInsert,
        FaultPoint::AfterInstallationReceiptInsert,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relay.sqlite3");
        let request = creation("txn_property_create_fault_00000000001");
        let store = RelayStore::open_with_faults(
            &database,
            TokenCipher::from_key([18; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
            Arc::new(FailAt(point)),
        )
        .unwrap();
        assert!(matches!(
            store.create_installation(&request, 1_000),
            Err(RelayError::InjectedFault)
        ));
        drop(store);
        let connection = Connection::open(&database).unwrap();
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM installations),
                        (SELECT COUNT(*) FROM installation_receipts)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0), "fault point {point:?}");
        drop(connection);
        let recovered = RelayStore::open(
            &database,
            TokenCipher::from_key([18; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        recovered.create_installation(&request, 1_001).unwrap();
    }
}

#[test]
fn every_ingest_fault_point_rolls_back_event_and_outbox_together() {
    for point in [
        FaultPoint::BeforeEventInsert,
        FaultPoint::AfterEventInsert,
        FaultPoint::AfterOutboxUpsert,
        FaultPoint::BeforeCommit,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relay.sqlite3");
        let store = RelayStore::open_with_faults(
            &database,
            TokenCipher::from_key([12; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
            Arc::new(FailAt(point)),
        )
        .unwrap();
        let installation = store
            .create_installation(&creation("txn_property_fault_000000000000001"), 1_000)
            .unwrap();
        store
            .register_device(
                &installation.installation_id,
                &capability(&installation.manage_capability),
                PushProviderKind::Apns,
                PushEnvironment::Sandbox,
                "apns-fault-token-0000000000",
                1_000,
            )
            .unwrap();
        assert!(matches!(
            store.ingest_event(
                &installation.installation_id,
                &capability(&installation.write_capability),
                &event(1, 30_000),
                1_000,
            ),
            Err(RelayError::InjectedFault)
        ));
        let diagnostics = store.diagnostics().unwrap();
        assert_eq!(diagnostics.retained_events, 0, "fault point {point:?}");
        assert_eq!(diagnostics.pending_outbox, 0, "fault point {point:?}");
        drop(store);
        let recovered = RelayStore::open(
            &database,
            TokenCipher::from_key([12; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        let response = recovered
            .ingest_event(
                &installation.installation_id,
                &capability(&installation.write_capability),
                &event(1, 30_000),
                1_001,
            )
            .unwrap();
        assert_eq!(response.cursor, 1, "fault point {point:?}");
        assert!(!response.replayed, "fault point {point:?}");
    }
}

#[test]
fn snapshot_revision_is_monotonic_and_event_transactional() {
    let directory = tempfile::tempdir().unwrap();
    let store = RelayStore::open(
        directory.path().join("relay.sqlite3"),
        TokenCipher::from_key([13; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .unwrap();
    let installation = store
        .create_installation(&creation("txn_property_snapshot_0000000000001"), 1_000)
        .unwrap();
    let write = capability(&installation.write_capability);
    let read = capability(&installation.read_capability);
    let mut first = event(1, 30_000);
    first.snapshot = Some(SnapshotUpdate {
        revision: 2,
        expires_at_ms: 40_000,
        ciphertext: URL_SAFE_NO_PAD.encode([90_u8; 64]),
    });
    store
        .ingest_event(&installation.installation_id, &write, &first, 1_000)
        .unwrap();
    let snapshot = store
        .fetch_snapshot(&installation.installation_id, &read, 1_001)
        .unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.through_cursor, 1);

    let mut stale = event(2, 30_000);
    stale.snapshot = Some(SnapshotUpdate {
        revision: 1,
        expires_at_ms: 40_000,
        ciphertext: URL_SAFE_NO_PAD.encode([91_u8; 64]),
    });
    assert!(matches!(
        store.ingest_event(&installation.installation_id, &write, &stale, 1_002),
        Err(RelayError::Conflict)
    ));
    assert_eq!(store.diagnostics().unwrap().retained_events, 1);
}

#[test]
fn token_rotation_is_generation_bound_and_stale_delete_is_harmless() {
    let directory = tempfile::tempdir().unwrap();
    let store = RelayStore::open(
        directory.path().join("relay.sqlite3"),
        TokenCipher::from_key([14; 32]),
        StoreLimits::default(),
        Arc::new(RelayMetrics::default()),
    )
    .unwrap();
    let installation = store
        .create_installation(&creation("txn_property_rotation_0000000000001"), 1_000)
        .unwrap();
    let manage = capability(&installation.manage_capability);
    let first = store
        .register_device(
            &installation.installation_id,
            &manage,
            PushProviderKind::Apns,
            PushEnvironment::Production,
            "apns-production-token-generation-one",
            1_000,
        )
        .unwrap();
    let duplicate = store
        .register_device(
            &installation.installation_id,
            &manage,
            PushProviderKind::Apns,
            PushEnvironment::Production,
            "apns-production-token-generation-one",
            1_001,
        )
        .unwrap();
    assert_eq!(duplicate.generation, 1);
    assert!(!duplicate.replaced);
    let rotated = store
        .register_device(
            &installation.installation_id,
            &manage,
            PushProviderKind::Apns,
            PushEnvironment::Production,
            "apns-production-token-generation-two",
            1_002,
        )
        .unwrap();
    assert_eq!(rotated.registration_id, first.registration_id);
    assert_eq!(rotated.generation, 2);
    assert!(rotated.replaced);
    store
        .tombstone_registration(
            &installation.installation_id,
            &manage,
            &rotated.registration_id,
            1,
            1_003,
        )
        .unwrap();
    assert_eq!(store.diagnostics().unwrap().active_registrations, 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn arbitrary_exact_replay_histories_never_create_cursor_gaps(history in prop::collection::vec(0_u8..16, 1..96)) {
        let directory = tempfile::tempdir().unwrap();
        let store = RelayStore::open(
            directory.path().join("relay.sqlite3"),
            TokenCipher::from_key([15; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        ).unwrap();
        let installation = store
            .create_installation(&creation("txn_property_replays_00000000000001"), 1_000)
            .unwrap();
        let write = capability(&installation.write_capability);
        let mut cursors = HashMap::new();
        for index in history {
            let index = usize::from(index);
            let response = store.ingest_event(
                &installation.installation_id,
                &write,
                &event(index, 30_000),
                1_000,
            ).unwrap();
            if let Some(previous) = cursors.insert(index, response.cursor) {
                prop_assert_eq!(previous, response.cursor);
                prop_assert!(response.replayed);
            }
        }
        let cursor_set: BTreeSet<_> = cursors.values().copied().collect();
        prop_assert_eq!(cursor_set, (1..=cursors.len() as u64).collect());
        prop_assert_eq!(store.diagnostics().unwrap().retained_events, cursors.len() as u64);
    }
}
