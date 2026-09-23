//! `broker blockers` / `broker unblock`: one registry across the broker's
//! stores, one id namespace, and one clearing command per blocker.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{
    Broker, GateFailureClass, GateStatus, HostOperationGuard, HostResourceCoordinator,
    HostResourceKind, HostResourceRequest, HostResourceRequirement, NewCoordinatedOperation,
    NewGateResult, OperationEffect, OperationIdentityProvenance, OperationProvider,
    OperationStatus,
};
use sha2::{Digest, Sha256};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    state: PathBuf,
    session_id: i64,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let worktree = tmp.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "work",
                worktree.to_str().unwrap(),
            ],
        );
        let state = tmp.path().join("host-state");
        std::fs::create_dir(&state).unwrap();
        let mut broker = Broker::open(&repo).unwrap();
        let session_id = broker.adopt(&worktree, None).unwrap().id;
        Self {
            _tmp: tmp,
            repo,
            worktree,
            state,
            session_id,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .output()
            .unwrap()
    }

    fn blockers(&self) -> Vec<serde_json::Value> {
        let output = self.run(&["blockers", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        report["blockers"].as_array().cloned().unwrap_or_default()
    }

    fn blocker(&self, id: &str) -> Option<serde_json::Value> {
        self.blockers()
            .into_iter()
            .find(|blocker| blocker["id"] == id)
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo).unwrap()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn an_outcome_unknown_operation_is_listed_and_needs_an_operator_outcome() {
    let fixture = Fixture::new();
    let mut broker = fixture.broker();
    let repository = format!("local:{}", broker.main_root().display());
    let operation = broker
        .store()
        .create_coordinated_operation(&NewCoordinatedOperation {
            session_id: fixture.session_id,
            provider: OperationProvider::Git,
            repository,
            scope: "repository".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("test".into()),
            command_json: r#"["git","push"]"#.into(),
            pid: dead_pid() as i64,
            host_operation_id: None,
            identity_provenance: OperationIdentityProvenance::LocalRepository,
        })
        .unwrap();
    broker
        .store()
        .transition_coordinated_operation(operation.id, OperationStatus::Running, None, None)
        .unwrap();
    broker
        .store()
        .transition_coordinated_operation(operation.id, OperationStatus::OutcomeUnknown, None, None)
        .unwrap();
    drop(broker);

    let id = format!("op:{}", operation.id);
    let blocker = fixture
        .blocker(&id)
        .expect("outcome_unknown op is a blocker");
    assert_eq!(blocker["kind"], "operation");
    assert_eq!(blocker["scope"], "repo");
    assert_eq!(blocker["safe_to_clear_automatically"], false);
    assert!(blocker["clear"].as_str().unwrap().contains("--outcome"));

    // No outcome: refused with exit 3, naming the flag, and nothing changes.
    let refused = fixture.run(&["unblock", &id, "--json"]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    let refusal: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refusal["needs_outcome"], true);
    assert!(
        refusal["required_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|flag| flag.as_str().unwrap().starts_with("--outcome"))
    );
    let text = fixture.run(&["unblock", &id]);
    assert_eq!(text.status.code(), Some(3));
    assert!(stderr(&text).contains("--outcome"), "{}", stderr(&text));
    assert!(
        fixture.blocker(&id).is_some(),
        "a refusal must change nothing"
    );

    let cleared = fixture.run(&[
        "unblock",
        &id,
        "--outcome",
        "failed",
        "--reason",
        "ls-remote shows the branch unchanged",
    ]);
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr(&cleared));
    assert!(fixture.blocker(&id).is_none());
    let row = fixture
        .broker()
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::ReconciledFailed);
}

#[test]
fn a_host_operation_is_addressed_by_its_hex_id() {
    let fixture = Fixture::new();
    let database = fixture.state.join("host-operations.db");
    let remote_key = format!("local:{}", fixture.broker().main_root().display());
    let hex = {
        let mut guard = HostOperationGuard::begin(
            &database,
            &remote_key,
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .unwrap();
        guard.mark_running().unwrap();
        let hex = guard.operation().operation_id.clone();
        // A holder killed after starting the write never finishes the row,
        // and dropping a running guard leaves it `running` the same way.
        drop(guard);
        hex
    };
    // The next writer on the same remote converts the orphaned `running`
    // row to `outcome_unknown` and is blocked by it (#276).
    assert!(
        HostOperationGuard::begin(
            &database,
            &remote_key,
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .is_err()
    );

    let id = format!("hostop:{hex}");
    let blocker = fixture.blocker(&id).expect("host operation is a blocker");
    assert_eq!(blocker["kind"], "host_operation");
    assert_eq!(blocker["scope"], "host");

    let refused = fixture.run(&["unblock", &id, "--json"]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    let cleared = fixture.run(&[
        "unblock",
        &id,
        "--outcome",
        "succeeded",
        "--reason",
        "the pushed ref is on the remote",
    ]);
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr(&cleared));
    assert!(fixture.blocker(&id).is_none());
    assert_eq!(
        aethyme_broker::host_operation(&database, &hex)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::ReconciledSucceeded
    );
}

#[test]
fn a_cached_failing_verdict_can_be_invalidated_so_the_gate_runs_fresh() {
    let fixture = Fixture::new();
    let tree = "0123456789abcdef0123456789abcdef01234567";
    let mut broker = fixture.broker();
    for (gate, status, class) in [
        (
            "unit",
            GateStatus::Fail,
            Some(GateFailureClass::TestFailure),
        ),
        ("lint", GateStatus::Pass, None),
    ] {
        broker
            .store()
            .record_gate_result(&NewGateResult {
                gate_name: gate.into(),
                tree_hash: tree.into(),
                definition_hash: "def".into(),
                status,
                failure_class: class,
                exit_code: Some(1),
                duration_ms: Some(10),
                wait_duration_ms: None,
                first_output_ms: None,
                output_bytes: None,
                log_path: None,
                session_id: Some(fixture.session_id),
            })
            .unwrap();
    }
    drop(broker);

    let id = format!("gatecache:unit@{tree}");
    let blocker = fixture
        .blocker(&id)
        .expect("cached failing verdict is listed");
    assert_eq!(blocker["kind"], "gate_cache");
    assert_eq!(blocker["safe_to_clear_automatically"], false);
    assert!(
        fixture.blocker(&format!("gatecache:lint@{tree}")).is_none(),
        "a cached pass blocks nothing"
    );

    let refused = fixture.run(&["unblock", &id]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("--reason"),
        "{}",
        stderr(&refused)
    );

    let cleared = fixture.run(&["unblock", &id, "--reason", "disk was full", "--json"]);
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr(&cleared));
    let mut broker = fixture.broker();
    // The lookup the runner uses now misses, so the next run executes.
    assert!(
        broker
            .store()
            .cached_gate_result_for_definition("unit", tree, "def")
            .unwrap()
            .is_none()
    );
    // Exactly that verdict: the passing verdict for another gate survives.
    assert!(
        broker
            .store()
            .cached_gate_result_for_definition("lint", tree, "def")
            .unwrap()
            .is_some()
    );
    let events = broker
        .store()
        .events_after_filtered(0, 1000, Some(aethyme_broker::BLOCKER_CLEARED))
        .unwrap();
    assert_eq!(events.len(), 1, "the invalidation is audited");
    assert!(
        events[0]
            .payload_json
            .as_deref()
            .unwrap()
            .contains("disk was full")
    );
    assert!(fixture.blocker(&id).is_none());
}

#[test]
fn a_stale_pidfile_whose_process_is_gone_is_cleared_and_a_live_one_is_not() {
    let fixture = Fixture::new();
    let run_dir = fixture.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(&run_dir).unwrap();
    let stale = run_dir.join(format!("{}-unit.pid", fixture.session_id));
    std::fs::write(&stale, format!("{} abc", dead_pid())).unwrap();
    let live = run_dir.join(format!("{}-lint.pid", fixture.session_id));
    // The current format: `<pgid> <tree> <pid> <start>`; the stale one above
    // uses the older two-field form, so both are read.
    let me = std::process::id();
    std::fs::write(&live, format!("{me} abc {me} 1")).unwrap();

    let id = format!("pidfile:{}-unit", fixture.session_id);
    let blocker = fixture.blocker(&id).expect("stale pidfile is a blocker");
    assert_eq!(blocker["safe_to_clear_automatically"], true);
    let live_id = format!("pidfile:{}-lint", fixture.session_id);
    assert!(fixture.blocker(&live_id).is_none());

    let cleared = fixture.run(&["unblock", &id]);
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr(&cleared));
    assert!(!stale.exists());
    let refused = fixture.run(&["unblock", &live_id]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(live.exists(), "a live pidfile is never removed");
}

#[test]
fn a_resource_lease_owned_by_a_closed_session_is_cleared() {
    let fixture = Fixture::new();
    let worktree_path = fixture
        .broker()
        .store()
        .session(fixture.session_id)
        .unwrap()
        .worktree_path;
    let fingerprint = format!("{:x}", Sha256::digest(worktree_path.as_bytes()));
    let mut coordinator =
        HostResourceCoordinator::open(&fixture.state.join("host-resources.db")).unwrap();
    let grant = coordinator
        .acquire(&HostResourceRequest {
            schema_version: aethyme_broker::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: "blockers-test".into(),
            repository: "unrelated-origin".into(),
            worktree_fingerprint: fingerprint,
            run_id: "run-1".into(),
            ttl_seconds: 600,
            holder_pid: Some(dead_pid()),
            resources: vec![HostResourceRequirement {
                key: "db".into(),
                resource: HostResourceKind::Namespace {
                    prefix: "quality".into(),
                },
            }],
        })
        .unwrap();
    coordinator
        .quarantine(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();
    drop(coordinator);

    let id = format!("resource:{}", grant.lease.lease_id);
    let open = fixture
        .blocker(&id)
        .expect("quarantined lease is a blocker");
    assert_eq!(open["session_id"], fixture.session_id);
    assert_eq!(
        open["safe_to_clear_automatically"], false,
        "an open session's lease may still front residue"
    );
    let refused = fixture.run(&["unblock", &id]);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("--confirm"),
        "{}",
        stderr(&refused)
    );

    fixture.broker().close(fixture.session_id).unwrap();
    let closed = fixture.blocker(&id).expect("still quarantined");
    assert_eq!(closed["safe_to_clear_automatically"], true);
    let cleared = fixture.run(&["unblock", &id]);
    assert_eq!(cleared.status.code(), Some(0), "{}", stderr(&cleared));
    assert!(fixture.blocker(&id).is_none());
    let _ = &fixture.worktree;
}

#[test]
fn status_json_carries_the_blockers_and_advice_names_unblock() {
    let fixture = Fixture::new();
    let run_dir = fixture.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join(format!("{}-unit.pid", fixture.session_id)),
        format!("{} abc", dead_pid()),
    )
    .unwrap();

    let output = fixture.run(&["status", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let blockers = status["blockers"].as_array().expect("blockers array");
    let id = format!("pidfile:{}-unit", fixture.session_id);
    assert!(blockers.iter().any(|blocker| blocker["id"] == id.as_str()));
    let advice = status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .find(|advice| advice["id"] == "blockers.present")
        .expect("status advice for blockers");
    assert!(
        advice["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command.as_str().unwrap() == format!("aethyme broker unblock {id}"))
    );
}
