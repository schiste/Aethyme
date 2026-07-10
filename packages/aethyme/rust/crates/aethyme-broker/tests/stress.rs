//! Multi-process stress test (issue #6): N real OS processes hammer one
//! broker.db concurrently. Design target is 15 concurrent sessions; this
//! test runs 20 for margin.
//!
//! Asserts: no worker errors (busy timeouts must be absorbed, never leak),
//! no lost writes (every marker event present exactly once), and strictly
//! increasing event ids.

use std::collections::HashSet;
use std::process::Command;

use aethyme_broker::BrokerStore;

const WORKERS: u32 = 20;
const OPS_PER_WORKER: u32 = 25;

#[test]
fn twenty_concurrent_writer_processes_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("broker.db");

    // Pre-create the database so workers race on writes, not on first-time
    // migration alone (migration racing is covered too — workers still all
    // call migrate on open).
    drop(BrokerStore::open(&db_path).unwrap());

    let worker_bin = env!("CARGO_BIN_EXE_broker-stress-worker");
    let mut children = Vec::new();
    for worker_id in 0..WORKERS {
        let child = Command::new(worker_bin)
            .arg(&db_path)
            .arg(worker_id.to_string())
            .arg(OPS_PER_WORKER.to_string())
            .spawn()
            .expect("spawn stress worker");
        children.push((worker_id, child));
    }

    for (worker_id, mut child) in children {
        let status = child.wait().expect("wait for stress worker");
        assert!(
            status.success(),
            "worker {worker_id} exited with {status:?} — a write failed or SQLITE_BUSY leaked"
        );
    }

    let store = BrokerStore::open(&db_path).unwrap();

    // Every session registered exactly once.
    let sessions = store.live_sessions().unwrap();
    assert_eq!(sessions.len(), WORKERS as usize);

    // Every marker event survived, exactly once, in strictly increasing
    // id order.
    let events = store.events_after(0, i64::MAX).unwrap();
    let mut seen = HashSet::new();
    let mut last_id = 0;
    let mut markers = 0u32;
    for event in &events {
        assert!(event.id > last_id, "event ids must be strictly increasing");
        last_id = event.id;
        if event.kind == "stress.marker" {
            markers += 1;
            let payload = event.payload_json.as_deref().unwrap();
            assert!(
                seen.insert(payload.to_string()),
                "duplicate marker payload: {payload}"
            );
        }
    }
    assert_eq!(
        markers,
        WORKERS * OPS_PER_WORKER,
        "lost writes: expected {} markers, found {markers}",
        WORKERS * OPS_PER_WORKER
    );
}

#[test]
fn concurrent_fresh_database_migration_races_safely() {
    // All workers open a database that does not exist yet — the migration
    // itself is the contended operation.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("fresh.db");
    let worker_bin = env!("CARGO_BIN_EXE_broker-stress-worker");

    let children: Vec<_> = (0..8u32)
        .map(|worker_id| {
            Command::new(worker_bin)
                .arg(&db_path)
                .arg(worker_id.to_string())
                .arg("1")
                .spawn()
                .expect("spawn stress worker")
        })
        .collect();
    for mut child in children {
        assert!(
            child.wait().unwrap().success(),
            "migration race broke a worker"
        );
    }

    let store = BrokerStore::open(&db_path).unwrap();
    assert_eq!(store.live_sessions().unwrap().len(), 8);
}
