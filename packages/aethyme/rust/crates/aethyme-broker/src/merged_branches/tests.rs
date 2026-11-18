use super::*;

use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn branch_exists(root: &Path, name: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .current_dir(root)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn the_rule_deletes_only_merged_unused_branches_of_cleaned_sessions() {
    let base = BranchFacts {
        session_not_cleaned: false,
        checked_out: false,
        recovery_archive: false,
        merged: Some(true),
    };
    assert_eq!(judge(base), Verdict::Delete);
    assert_eq!(
        judge(BranchFacts {
            session_not_cleaned: true,
            ..base
        }),
        Verdict::Keep("session_not_cleaned")
    );
    assert_eq!(
        judge(BranchFacts {
            checked_out: true,
            ..base
        }),
        Verdict::Keep("checked_out")
    );
    assert_eq!(
        judge(BranchFacts {
            recovery_archive: true,
            ..base
        }),
        Verdict::Keep("recovery_archive")
    );
    assert_eq!(
        judge(BranchFacts {
            merged: Some(false),
            ..base
        }),
        Verdict::Keep("unmerged")
    );
    assert_eq!(
        judge(BranchFacts {
            merged: None,
            ..base
        }),
        Verdict::Keep("unknown")
    );
}

struct Fixture {
    _owner: tempfile::TempDir,
    _storage: crate::test_storage::TestStorage,
    root: std::path::PathBuf,
    broker: crate::Broker,
}

fn fixture() -> Fixture {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&root, &["add", ".gitignore", "base.txt"]);
    git(&root, &["commit", "-qm", "fixture"]);
    aethyme_testkit::ensure_origin(&root);
    git(&root, &["remote", "set-head", "origin", "main"]);
    let storage = crate::test_storage::TestStorage::new();
    let broker = storage.open(&root).unwrap();
    Fixture {
        _owner: owner,
        _storage: storage,
        root,
        broker,
    }
}

impl Fixture {
    /// A session on `branch`, cleaned unless `open`.
    fn session(&mut self, branch: &str, open: bool) -> i64 {
        let worktree = self.root.with_file_name(branch.replace('/', "-"));
        let session = self
            .broker
            .store()
            .register_session(&crate::NewSession {
                worktree_path: worktree.to_string_lossy().into_owned(),
                branch: branch.into(),
                origin: crate::SessionOrigin::Adopted,
                task: Some(format!("work on {branch}")),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap();
        if !open {
            self.broker
                .store()
                .set_session_status(session.id, crate::SessionStatus::Cleaned, None)
                .unwrap();
        }
        session.id
    }
}

#[test]
fn a_slice_deletes_merged_branches_of_cleaned_sessions_and_keeps_the_rest() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    git(&root, &["branch", "agent/open"]);
    git(&root, &["branch", "agent/checked-out"]);
    git(&root, &["branch", "agent/archived"]);
    git(&root, &["branch", "unrelated"]);
    git(&root, &["switch", "-q", "-c", "agent/unmerged"]);
    std::fs::write(root.join("work.txt"), "unmerged work\n").unwrap();
    git(&root, &["add", "work.txt"]);
    git(&root, &["commit", "-qm", "unmerged work"]);
    git(&root, &["switch", "-q", "main"]);
    let checkout = root.with_file_name("checked-out");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            checkout.to_str().unwrap(),
            "agent/checked-out",
        ],
    );

    fixture.session("agent/merged", false);
    fixture.session("agent/unmerged", false);
    fixture.session("agent/open", true);
    fixture.session("agent/checked-out", false);
    let archived = fixture.session("agent/archived", false);
    let archive_root = fixture.broker.recovery_archive_root().unwrap();
    std::fs::create_dir_all(archive_root.join(format!("{archived}-archive"))).unwrap();

    let summary = fixture.broker.run_merged_branch_lane();

    assert_eq!(summary.skipped, None, "{summary:?}");
    assert_eq!(summary.deleted, 1, "{summary:?}");
    assert_eq!(summary.deleted_total, 1);
    assert_eq!(summary.cleaned_session_branches, 4, "{summary:?}");
    let kept = |reason: &str| summary.kept_by_reason.get(reason).copied();
    assert_eq!(kept("unmerged"), Some(1), "{summary:?}");
    assert_eq!(kept("session_not_cleaned"), Some(1), "{summary:?}");
    assert_eq!(kept("checked_out"), Some(1), "{summary:?}");
    assert_eq!(kept("recovery_archive"), Some(1), "{summary:?}");
    assert!(!branch_exists(&root, "agent/merged"));
    for kept in [
        "agent/unmerged",
        "agent/open",
        "agent/checked-out",
        "agent/archived",
        "unrelated",
        "main",
    ] {
        assert!(branch_exists(&root, kept), "{kept} must stay");
    }
    let advice = status_advice(fixture.broker.store_ref()).unwrap();
    assert_eq!(advice.id, ADVICE_ID);
    assert!(
        advice.summary.contains("4 local branch(es)"),
        "{}",
        advice.summary
    );
}

#[test]
fn a_branch_that_moved_after_it_was_judged_is_kept() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/moving"]);
    fixture.session("agent/moving", false);
    let repo = crate::GitRepo::discover(&root).unwrap();
    let judged = git(&root, &["rev-parse", "agent/moving"]);
    // Somebody commits on the branch between the verdict and the delete.
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let moved = git(&root, &["commit-tree", &tree, "-p", &judged, "-m", "late"]);
    git(&root, &["update-ref", "refs/heads/agent/moving", &moved]);
    assert!(
        repo.delete_branch_ref_checked("agent/moving", &judged)
            .is_err()
    );
    assert!(branch_exists(&root, "agent/moving"));
}

#[test]
fn the_lane_can_be_switched_off() {
    let mut fixture = fixture();
    let root = fixture.root.clone();
    git(&root, &["branch", "agent/merged"]);
    fixture.session("agent/merged", false);
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[cleanup]\ndelete_merged_session_branches = false\n",
    )
    .unwrap();

    let summary = fixture.broker.run_merged_branch_lane();
    assert_eq!(summary.skipped.as_deref(), Some("disabled"));
    assert_eq!(summary.deleted, 0);
    assert!(branch_exists(&root, "agent/merged"));
}
