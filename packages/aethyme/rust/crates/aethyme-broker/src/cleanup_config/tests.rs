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

const OLD_CONFIG: &str = "[cleanup]\nstrict = false\n";
const NEW_CONFIG: &str = "[cleanup]\nstrict = false\nvenv_markers = [\".env-check.json\"]\n";

struct Repo {
    _owner: tempfile::TempDir,
    root: PathBuf,
    /// The commit the primary checkout is held at.
    old: String,
    /// The default branch as last fetched.
    new: String,
}

/// A primary checkout held at `old`, whose fetched default branch
/// (`origin/main`, named by `origin/HEAD`) is `new`, one commit ahead that
/// writes `default_config` as the config.
fn behind_repository(default_config: &str) -> Repo {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("repository");
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".aethyme/config.toml"), OLD_CONFIG).unwrap();
    git(&root, &["add", ".aethyme/config.toml"]);
    git(&root, &["commit", "-qm", "old"]);
    let old = git(&root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join(".aethyme/config.toml"), default_config).unwrap();
    git(&root, &["commit", "-qam", "new"]);
    let new = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["update-ref", "refs/remotes/origin/main", &new]);
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&root, &["reset", "-q", "--hard", &old]);
    Repo {
        _owner: owner,
        root,
        old,
        new,
    }
}

#[test]
fn a_behind_unedited_checkout_reads_the_default_branch_blob() {
    let repo = behind_repository(NEW_CONFIG);
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.reason, "default_branch");
    assert!(resolution.default_differs);
    assert_eq!(resolution.default_ref.as_deref(), Some("origin/main"));
    assert_eq!(
        resolution.default_commit.as_deref(),
        Some(repo.new.as_str())
    );
    match &resolution.source {
        CleanupConfigSource::DefaultBranch { text, .. } => {
            assert_eq!(text.as_deref(), Some(NEW_CONFIG));
        }
        other => panic!("expected the default branch, got {other:?}"),
    }
    assert_eq!(resolution.counter(), "cleanup_config_source_default_branch");

    // Outside a run scope every reader keeps the working copy.
    assert!(crate::auto_cleanup::venv_markers(&repo.root).is_empty());
    let recorder = crate::phase_telemetry::Recorder::default();
    {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let _scope = RunScope::enter(&repo.root);
        // The `[cleanup]` readers see the default branch's setting, also
        // from another thread, and nested scopes share the resolution.
        assert_eq!(
            crate::auto_cleanup::venv_markers(&repo.root),
            [".env-check.json"]
        );
        let root = repo.root.clone();
        let from_thread = std::thread::spawn(move || crate::auto_cleanup::venv_markers(&root))
            .join()
            .unwrap();
        assert_eq!(from_thread, [".env-check.json"]);
        let _nested = RunScope::enter(&repo.root);
        assert!(crate::AutoCleanupPolicy::load(&repo.root).is_ok());
    }
    let (millis, counts) = recorder.totals();
    assert_eq!(counts.get("cleanup_config_source_default_branch"), Some(&1));
    assert_eq!(counts.get("cleanup_config_resolve"), Some(&1));
    assert!(millis.contains_key("cleanup_config_resolve"));
    // Dropped: the working copy again.
    assert!(crate::auto_cleanup::venv_markers(&repo.root).is_empty());
    assert_eq!(git(&repo.root, &["rev-parse", "HEAD"]), repo.old);
}

#[test]
fn a_locally_edited_config_keeps_the_working_copy() {
    let repo = behind_repository(NEW_CONFIG);
    std::fs::write(
        repo.root.join(".aethyme/config.toml"),
        "[cleanup]\nstrict = true\n",
    )
    .unwrap();
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.source, CleanupConfigSource::WorkingCopy);
    assert_eq!(resolution.reason, "local_edit");
    assert!(resolution.default_differs);
    assert_eq!(
        resolution.counter(),
        "cleanup_config_source_working_copy_local_edit"
    );
    let _scope = RunScope::enter(&repo.root);
    assert!(crate::AutoCleanupPolicy::load(&repo.root).unwrap().strict);
    assert!(crate::auto_cleanup::venv_markers(&repo.root).is_empty());
}

#[test]
fn a_working_copy_already_equal_to_the_default_branch_is_not_a_difference() {
    let repo = behind_repository(NEW_CONFIG);
    std::fs::write(repo.root.join(".aethyme/config.toml"), NEW_CONFIG).unwrap();
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.source, CleanupConfigSource::WorkingCopy);
    assert_eq!(resolution.reason, "same");
    assert!(!resolution.default_differs);
}

#[test]
fn a_diverged_checkout_keeps_the_working_copy() {
    let repo = behind_repository(NEW_CONFIG);
    std::fs::write(repo.root.join("local.txt"), "local\n").unwrap();
    git(&repo.root, &["add", "local.txt"]);
    git(&repo.root, &["commit", "-qm", "local"]);
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.source, CleanupConfigSource::WorkingCopy);
    assert_eq!(resolution.reason, "diverged");
    assert!(resolution.default_differs);
    let _scope = RunScope::enter(&repo.root);
    assert!(crate::auto_cleanup::venv_markers(&repo.root).is_empty());
}

#[test]
fn a_missing_default_branch_ref_keeps_the_working_copy() {
    let repo = behind_repository(NEW_CONFIG);
    git(
        &repo.root,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    git(
        &repo.root,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.source, CleanupConfigSource::WorkingCopy);
    assert_eq!(resolution.reason, "no_default_branch");
    assert!(!resolution.default_differs);
    let _scope = RunScope::enter(&repo.root);
    assert!(crate::auto_cleanup::venv_markers(&repo.root).is_empty());
}

#[test]
fn an_unparsable_default_branch_blob_keeps_the_working_copy() {
    let repo = behind_repository("[cleanup\nstrict = true\n");
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.source, CleanupConfigSource::WorkingCopy);
    assert_eq!(resolution.reason, "default_branch_unparsable");
    assert!(resolution.default_differs);
    let _scope = RunScope::enter(&repo.root);
    let policy = crate::AutoCleanupPolicy::load(&repo.root).unwrap();
    assert!(!policy.strict);
}

#[test]
fn an_invalid_default_branch_cleanup_table_keeps_the_working_copy() {
    let repo = behind_repository("[cleanup]\nvenv_markers = [\".env\"]\n");
    let resolution = resolve(&repo.root);
    assert_eq!(resolution.reason, "default_branch_unparsable");
}

#[test]
fn status_advises_only_when_the_working_copy_was_kept_over_a_different_default() {
    let repo = behind_repository(NEW_CONFIG);
    let database = tempfile::tempdir().unwrap();
    let store = &crate::BrokerStore::open(&database.path().join("broker.db")).unwrap();
    // No run recorded yet, and nothing recorded without a scope.
    record_active(store, &repo.root);
    assert!(status_advice(store).is_none());
    {
        let _scope = RunScope::enter(&repo.root);
        record_active(store, &repo.root);
    }
    // The default branch was read: nothing to advise.
    assert!(status_advice(store).is_none());

    std::fs::write(
        repo.root.join(".aethyme/config.toml"),
        "[cleanup]\nstrict = true\n",
    )
    .unwrap();
    {
        let _scope = RunScope::enter(&repo.root);
        record_active(store, &repo.root);
    }
    let advice = status_advice(store).expect("advice");
    assert_eq!(advice.id, ADVICE_ID);
    assert!(advice.summary.contains("local_edit"), "{}", advice.summary);
    assert!(advice.summary.contains("origin/main"), "{}", advice.summary);
    assert!(advice.commands[0].contains("local edit"));
}
