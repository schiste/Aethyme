//! Worker process for the multi-process stress test (`tests/stress.rs`).
//!
//! Usage: broker-stress-worker <db-path> <worker-id> <ops>
//!
//! Each op registers/updates broker state and appends a marker event whose
//! payload uniquely identifies (worker, op). The test asserts every marker
//! survived — lost writes, SQLITE_BUSY leaks, or ordering violations fail
//! the run. Exits non-zero on the first error.

use std::path::PathBuf;

use aethyme_broker::{BrokerStore, NewSession, SessionOrigin};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: broker-stress-worker <db-path> <worker-id> <ops>");
        std::process::exit(2);
    }
    let db_path = PathBuf::from(&args[1]);
    let worker_id: u32 = args[2].parse().expect("worker-id must be an integer");
    let ops: u32 = args[3].parse().expect("ops must be an integer");

    let mut store = match BrokerStore::open(&db_path) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("worker {worker_id}: open failed: {err}");
            std::process::exit(1);
        }
    };

    // One session per worker — exercises the partial unique index and the
    // register+event transaction under contention.
    let session = match store.register_session(&NewSession {
        worktree_path: format!("/stress/worktree-{worker_id}"),
        branch: format!("agent/stress-{worker_id}"),
        origin: SessionOrigin::Adopted,
        task: Some(format!("stress worker {worker_id}")),
        diff_base: None,
        adoption_base: None,
        adopted_head: None,
        repository_contract: None,
        pid: None,
        command: None,
        log_path: None,
    }) {
        Ok(session) => session,
        Err(err) => {
            eprintln!("worker {worker_id}: register failed: {err}");
            std::process::exit(1);
        }
    };

    for op in 0..ops {
        let result = store
            .set_implicit_leases(
                session.id,
                &[format!("src/worker-{worker_id}/file-{}.rs", op % 5)],
            )
            .and_then(|()| store.touch_session_activity(session.id, 1_000 + op as i64))
            .and_then(|()| {
                store.append_event(
                    "stress.marker",
                    Some(session.id),
                    Some(&format!("{{\"worker\":{worker_id},\"op\":{op}}}")),
                )
            });
        if let Err(err) = result {
            eprintln!("worker {worker_id}: op {op} failed: {err}");
            std::process::exit(1);
        }
    }
}
