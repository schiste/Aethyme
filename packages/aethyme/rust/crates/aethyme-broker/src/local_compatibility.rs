//! Executable Local compatibility schedules (#681). This is a test-only module;
//! it adds no alternate provider, capture, policy or authority implementation.
use crate::collaboration_state::{
    CollaborationRoot, CollaborationStateError, CollaborationStore, ProjectKey,
};
use crate::collaboration_submit::{self as capture, CaptureStatus, CaptureSubmit, Setting};
use crate::{Broker, CachePolicy, PromotionIntent};
use std::path::{Path, PathBuf};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
struct Fixture {
    repo: tempfile::TempDir,
    host: tempfile::TempDir,
    _worktrees: tempfile::TempDir,
    _origin: tempfile::TempDir,
    broker: Broker,
    session: i64,
    worktree: PathBuf,
}
impl Fixture {
    fn new(config: Option<&str>) -> Self {
        let repo = tempfile::Builder::new()
            .prefix("Playground-compatibility-")
            .tempdir()
            .unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("README.md"), "Playground\n").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(
            repo.path(),
            &[
                "commit",
                "-qm",
                "test(compatibility): initial Playground source",
            ],
        );
        if let Some(config) = config {
            std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
            std::fs::write(repo.path().join(".aethyme/config.toml"), config).unwrap();
            git(repo.path(), &["add", ".aethyme/config.toml"]);
            git(
                repo.path(),
                &[
                    "commit",
                    "-qm",
                    "test(compatibility): explicit Playground policy",
                ],
            );
        }
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
        git(repo.path(), &["remote", "set-head", "origin", "main"]);
        let host = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let mut broker = Broker::open(repo.path())
            .unwrap()
            .with_worktree_root(worktrees.path())
            .with_collaboration_state(host.path());
        let session = broker
            .start_worktree("compatibility", Some("disposable Playground compatibility"))
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("independent.txt"), "independent Local edit\n").unwrap();
        git(&worktree, &["add", "independent.txt"]);
        git(
            &worktree,
            &[
                "commit",
                "-qm",
                "test(compatibility): independent Local edit",
            ],
        );
        Self {
            repo,
            host,
            _worktrees: worktrees,
            _origin: origin,
            broker,
            session: session.id,
            worktree,
        }
    }
    fn submit(&mut self) -> CaptureSubmit {
        capture::submit(
            &mut self.broker,
            self.session,
            CachePolicy::Use,
            PromotionIntent::Configured,
        )
        .unwrap()
    }
}
#[test]
fn absent_or_disabled_collaboration_allocates_no_retained_state_and_keeps_legacy_verdict() {
    for config in [
        None,
        Some("[collaboration]\ncapture='off'\nproject='proj-disabled'\n"),
        Some("[collaboration]\ncapture='off'\nfuture_setting='unknown but disabled'\n"),
    ] {
        let mut f = Fixture::new(config);
        assert!(!f.host.path().join("collaboration").exists());
        let outcome = match f.submit() {
            CaptureSubmit::Submitted {
                outcome,
                capture: None,
            } => outcome,
            _ => panic!("disabled must preserve the legacy result"),
        };
        assert!(!outcome.no_changes);
        assert_eq!(
            outcome.entry.head_commit,
            git(&f.worktree, &["rev-parse", "HEAD"])
        );
        assert!(outcome.conflicts.is_empty());
        assert!(!f.host.path().join("collaboration").exists());
        assert!(outcome.gate_outcomes.is_empty());
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("collaboration_capture")
        );
    }
}
#[test]
fn advisory_capture_failure_preserves_real_legacy_submission() {
    let mut f = Fixture::new(Some(
        "[collaboration]\ncapture='advisory'\nproject='proj-compat'\n",
    ));
    std::fs::write(
        f.host.path().join("collaboration"),
        "blocked Local state path",
    )
    .unwrap();
    let (outcome, report) = match f.submit() {
        CaptureSubmit::Submitted {
            outcome,
            capture: Some(step),
        } => (outcome, step.finish(&f.broker)),
        _ => panic!("advisory must preserve actual legacy submit"),
    };
    assert!(!outcome.no_changes);
    assert!(outcome.conflicts.is_empty());
    assert_eq!(report.policy, "advisory");
    assert_eq!(report.status, CaptureStatus::Failed);
    assert!(report.receipt.is_none());
    assert_eq!(
        std::fs::read(f.host.path().join("collaboration")).unwrap(),
        b"blocked Local state path"
    );
}
#[test]
fn required_capture_failure_refuses_before_queue_or_integration_movement() {
    let mut f = Fixture::new(Some(
        "[collaboration]\ncapture='required'\nproject='proj-compat'\n",
    ));
    std::fs::write(
        f.host.path().join("collaboration"),
        "blocked Local state path",
    )
    .unwrap();
    let before = git(
        f.repo.path(),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/aethyme/integration",
        ],
    );
    let report = match f.submit() {
        CaptureSubmit::Refused(report) => report,
        _ => panic!("required must refuse before legacy submit"),
    };
    assert_eq!(report.policy, "required");
    assert_eq!(report.status, CaptureStatus::Failed);
    assert_eq!(
        before,
        git(
            f.repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/aethyme/integration"
            ]
        )
    );
    let queue: i64 = rusqlite::Connection::open(f.repo.path().join(".aethyme/broker.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM merge_queue", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queue, 0);
}
#[test]
fn unknown_policy_and_unknown_required_keys_do_not_negotiate_to_safe() {
    for config in [
        "[collaboration]\ncapture='future'\n",
        "[collaboration]\ncapture='required'\nproject='proj-compat'\nfuture_setting=true\n",
    ] {
        assert!(matches!(
            capture::setting_from_text(Some(config)),
            Setting::Unsupported { .. }
        ));
    }
    match capture::setting_from_text(Some(
        "[collaboration]\ncapture='advisory'\nproject='proj-compat'\nfuture_setting=true\n",
    )) {
        Setting::On { ignored, .. } => assert_eq!(ignored, vec!["future_setting"]),
        _ => panic!(),
    }
    let mut raw = serde_json::json!({"schema":"aethyme.contrib-context/v999","authority":"accepted","coverage":"complete"});
    assert!(
        aethyme_contracts::experimental_v0::Record::decode(
            &serde_json::to_vec(&raw).unwrap(),
            &[&crate::collaboration_context::CONTEXT_SCHEMA]
        )
        .is_err()
    );
    raw["schema"] = crate::collaboration_context::CONTEXT_SCHEMA_NAME.into();
    assert!(
        aethyme_contracts::experimental_v0::Record::decode(
            &serde_json::to_vec(&raw).unwrap(),
            &[&crate::collaboration_context::CONTEXT_SCHEMA]
        )
        .is_err()
    );
}
#[test]
fn newer_incompatible_store_refuses_without_migration_or_data_mutation() {
    let host = tempfile::tempdir().unwrap();
    let root = CollaborationRoot::under_host_state(host.path());
    let project = ProjectKey::parse("compatibility-migration").unwrap();
    let store = CollaborationStore::open(&root, &project, &[]).unwrap();
    let db = store.project_dir().join("state.db");
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE meta SET value='999' WHERE key IN ('schema_version','min_compatible_schema')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('compatibility-evidence','unchanged')",
        [],
    )
    .unwrap();
    let before: Vec<(String, String)> = conn
        .prepare("SELECT key,value FROM meta ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(matches!(
        CollaborationStore::open(&root, &project, &[]),
        Err(CollaborationStateError::SchemaTooNew { .. })
    ));
    let after: Vec<(String, String)> = conn
        .prepare("SELECT key,value FROM meta ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(before, after);
}
#[test]
fn overlapping_shared_state_root_refuses_with_a_separate_location_action() {
    let host = tempfile::tempdir().unwrap();
    let root = CollaborationRoot::under_host_state(host.path());
    let blocked = crate::collaboration_state::ForbiddenRoot {
        path: host.path().to_owned(),
        kind: crate::collaboration_state::ForbiddenKind::WorktreeContainer,
    };
    let result = CollaborationStore::open(
        &root,
        &ProjectKey::parse("unsafe-shared").unwrap(),
        &[blocked],
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unsafe cleanup overlap must refuse"),
    };
    assert!(error.to_string().contains("move"));
    assert!(!root.path().exists());
}
#[cfg(target_os = "macos")]
mod old_binary;
