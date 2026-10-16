//! T04: one logical operation cannot be rebound to another immutable revision.

use std::path::Path;
use std::process::Command;

use aethyme_broker::collaboration_archive::{CommitOid, pin_commit};
use aethyme_broker::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureReceipt, CaptureRequest, OperationId, RetentionBoundary,
    capture, receipt,
};
use aethyme_broker::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};

fn git(repo: &Path, args: &[&str]) {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    command.env_clear();
    if let Some(system_root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", system_root);
    }
    let output = command
        .env("PATH", std::env::var_os("PATH").expect("test PATH"))
        .env("HOME", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_SYSTEM", null)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "capture revision fixture")
        .env("GIT_AUTHOR_EMAIL", "capture-revision@example.invalid")
        .env("GIT_COMMITTER_NAME", "capture revision fixture")
        .env("GIT_COMMITTER_EMAIL", "capture-revision@example.invalid")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "init.templateDir=",
        ])
        .args(["-c", &format!("core.attributesFile={null}")])
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            repo.join("empty-hooks").display()
        ))
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn revisions(repo: &Path) -> [CommitOid; 3] {
    std::fs::create_dir(repo.join("empty-hooks")).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    let mut commits = Vec::new();
    for text in ["base\n", "first revision\n", "second revision\n"] {
        std::fs::write(repo.join("source.txt"), text).unwrap();
        git(repo, &["add", "source.txt"]);
        git(repo, &["commit", "-qm", "synthetic revision"]);
        commits.push(pin_commit(repo, "HEAD").unwrap());
    }
    commits.try_into().unwrap()
}

fn request(repo: &Path, operation: &str, base: &CommitOid, result: &CommitOid) -> CaptureRequest {
    CaptureRequest {
        operation_id: OperationId::parse(operation).unwrap(),
        repository: repo.to_path_buf(),
        base: base.clone(),
        result: result.clone(),
        policy: CapturePolicy::Advisory,
        retention: RetentionBoundary::UntilReleased,
    }
}

fn acknowledged(outcome: CaptureOutcome) -> CaptureReceipt {
    match outcome {
        CaptureOutcome::Acknowledged(receipt) => receipt,
        other => panic!("expected retained capture, got {other:?}"),
    }
}

fn replay_with_changed_revision(change_base: bool) {
    let repository = tempfile::tempdir().unwrap();
    let [base, middle, tip] = revisions(repository.path());
    let host = tempfile::tempdir().unwrap();
    let root = CollaborationRoot::under_host_state(host.path());
    let project = ProjectKey::parse("proj-t04-revisions").unwrap();
    let mut store = CollaborationStore::open(&root, &project, &[]).unwrap();
    let original = request(repository.path(), "logical-operation", &base, &tip);
    let first = acknowledged(capture(&mut store, &original).unwrap());
    assert_eq!(acknowledged(capture(&mut store, &original).unwrap()), first);

    let mut changed = original.clone();
    if change_base {
        changed.base = middle;
    } else {
        changed.result = middle;
    }
    // The changed pair is valid under a separate operation: this is an identity
    // conflict, rather than a missing object or an unsupported-history fixture.
    let mut independent = changed.clone();
    independent.operation_id = OperationId::parse("separate-operation").unwrap();
    let separate = acknowledged(capture(&mut store, &independent).unwrap());
    assert_ne!(separate.receipt_record_id, first.receipt_record_id);
    if change_base {
        assert_ne!(separate.base, first.base);
        assert_eq!(separate.result, first.result);
    } else {
        assert_eq!(separate.base, first.base);
        assert_ne!(separate.result, first.result);
    }

    assert_eq!(
        capture(&mut store, &changed).unwrap_err().code(),
        "conflicting_operation"
    );
    assert_eq!(
        receipt(&mut store, &original.operation_id).unwrap(),
        Some(first.clone())
    );
    assert_eq!(acknowledged(capture(&mut store, &original).unwrap()), first);
    drop(store);
    let mut reopened = CollaborationStore::open(&root, &project, &[]).unwrap();
    assert_eq!(
        receipt(&mut reopened, &original.operation_id).unwrap(),
        Some(first)
    );
}

#[test]
fn changing_only_the_base_revision_cannot_overwrite_an_operation_receipt() {
    replay_with_changed_revision(true);
}

#[test]
fn changing_only_the_result_revision_cannot_overwrite_an_operation_receipt() {
    replay_with_changed_revision(false);
}
