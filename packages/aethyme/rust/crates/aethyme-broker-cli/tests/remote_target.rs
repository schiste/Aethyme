use std::path::Path;
use std::process::Command;

use aethyme_broker::{GitRepo, RemoteAssertionEvidence, RemoteTargetError, RemoteUrlSyntax};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q"]);
    temp
}

#[test]
fn configured_https_fetch_and_ssh_push_resolve_to_one_redacted_target() {
    let temp = repository();
    git(
        temp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://reader:fetch-secret@GitHub.COM/Team/Project.git?token=hidden",
        ],
    );
    git(
        temp.path(),
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:Team/Project.git",
        ],
    );

    let target = GitRepo::discover(temp.path())
        .unwrap()
        .resolve_remote_target("origin", Some("Team/Project.git"))
        .unwrap();
    assert_eq!(target.normalized_host, "github.com");
    assert_eq!(target.repository_path, "Team/Project");
    assert_eq!(target.coordination_key, "github.com/team/project");
    assert_eq!(target.display_slug, "Team/Project");
    assert_eq!(target.remote_name, "origin");
    assert_eq!(target.fetch_url, "https://github.com/Team/Project");
    assert_eq!(target.push_url, "github.com:Team/Project");
    assert_eq!(target.caller_assertion.as_deref(), Some("Team/Project.git"));
    assert_eq!(target.evidence.fetch.syntax, RemoteUrlSyntax::Https);
    assert_eq!(target.evidence.push.syntax, RemoteUrlSyntax::Scp);
    assert_eq!(
        target.evidence.caller_assertion,
        RemoteAssertionEvidence::MatchedRepositoryPath
    );

    let serialized = serde_json::to_string(&target).unwrap();
    for secret in ["reader", "fetch-secret", "token", "hidden"] {
        assert!(
            !serialized.contains(secret),
            "leaked {secret}: {serialized}"
        );
    }
}

#[test]
fn configured_ssh_url_is_normalized_deterministically() {
    let temp = repository();
    git(
        temp.path(),
        &[
            "remote",
            "add",
            "publish",
            "SsH://writer:secret@Example.COM:22/group/repo.git#credential",
        ],
    );
    let target = GitRepo::discover(temp.path())
        .unwrap()
        .resolve_remote_target("publish", None)
        .unwrap();
    assert_eq!(target.normalized_host, "example.com");
    assert_eq!(target.coordination_key, "example.com/group/repo");
    assert_eq!(target.fetch_url, "ssh://example.com/group/repo");
    assert_eq!(target.fetch_url, target.push_url);
    assert_eq!(
        target.evidence.caller_assertion,
        RemoteAssertionEvidence::NotSupplied
    );
}

#[test]
fn mismatched_push_repository_and_caller_assertions_fail_closed() {
    let temp = repository();
    git(
        temp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://reader:fetch-secret@host.test/team/repo.git?token=hidden",
        ],
    );
    git(
        temp.path(),
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "ssh://git@host.test/team/other.git",
        ],
    );
    let repo = GitRepo::discover(temp.path()).unwrap();
    let mismatch = repo.resolve_remote_target("origin", None).unwrap_err();
    assert!(matches!(
        &mismatch,
        RemoteTargetError::FetchPushMismatch { .. }
    ));
    let mismatch = mismatch.to_string();
    for secret in ["reader", "fetch-secret", "token", "hidden"] {
        assert!(!mismatch.contains(secret), "leaked {secret}: {mismatch}");
    }

    git(
        temp.path(),
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "ssh://git@host.test/team/repo.git",
        ],
    );
    assert!(matches!(
        repo.resolve_remote_target("origin", Some("team/other")),
        Err(RemoteTargetError::AssertionMismatch { .. })
    ));
}
