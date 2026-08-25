use std::path::Path;
use std::process::Command;

use aethyme_testkit::invoke::Invoke;
use aethyme_testkit::tmp_dir;

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success());
}

fn repo() -> (tempfile::TempDir, i64) {
    let tmp = tmp_dir();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("tracked.txt"), "base\n").unwrap();
    git(tmp.path(), &["add", "tracked.txt"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let adopted = Invoke::new(["broker", "adopt", "--task", "operation test", "--json"])
        .cwd(tmp.path())
        .run();
    adopted.ok();
    let session = adopted.json()["id"].as_i64().unwrap();
    (tmp, session)
}

#[test]
fn coordinated_git_frontend_journals_a_read() {
    let (tmp, session) = repo();
    let session_arg = session.to_string();
    let result = Invoke::new([
        "broker",
        "git",
        "--session",
        &session_arg,
        "--json",
        "--",
        "status",
        "--short",
    ])
    .cwd(tmp.path())
    .run();
    result.ok();
    let payload = result.json();
    assert_eq!(payload["operation"]["provider"], "git");
    assert_eq!(payload["operation"]["effect"], "read");
    assert_eq!(payload["operation"]["status"], "succeeded");

    let journal = Invoke::new(["broker", "operations", "--json"])
        .cwd(tmp.path())
        .run();
    journal.ok();
    assert_eq!(journal.json().as_array().unwrap().len(), 1);
}

#[test]
fn github_and_destructive_frontends_fail_before_execution_without_required_scope() {
    let (tmp, session) = repo();
    let session_arg = session.to_string();
    let missing_repo = Invoke::new([
        "broker",
        "gh",
        "--session",
        &session_arg,
        "--",
        "pr",
        "view",
        "1",
    ])
    .cwd(tmp.path())
    .run();
    missing_repo.expect_code(1);
    missing_repo.assert_contains("broker gh requires --repo owner/name");

    let destructive = Invoke::new([
        "broker",
        "git",
        "--session",
        &session_arg,
        "--",
        "branch",
        "-D",
        "main",
    ])
    .cwd(tmp.path())
    .run();
    destructive.expect_code(1);
    destructive.assert_contains("requires --destructive");

    let missing_reason = Invoke::new([
        "broker",
        "git",
        "--session",
        &session_arg,
        "--",
        "branch",
        "coordinated",
    ])
    .cwd(tmp.path())
    .run();
    missing_reason.expect_code(1);
    missing_reason.assert_contains("require --reason");

    Invoke::new([
        "broker",
        "git",
        "--session",
        &session_arg,
        "--reason",
        "authorized CLI regression",
        "--",
        "branch",
        "coordinated",
    ])
    .cwd(tmp.path())
    .run()
    .ok();
}

#[test]
fn github_frontend_refuses_targets_after_the_broker_separator() {
    let (tmp, session) = repo();
    let session_arg = session.to_string();

    for (command, expected) in [
        (
            vec!["pr", "view", "1", "--repo", "Other/Repo"],
            "do not pass a second repository target after --",
        ),
        (
            vec!["api", "repos/Other/Repo/issues"],
            "does not match broker --repo",
        ),
    ] {
        let mut args = vec![
            "broker",
            "gh",
            "--session",
            &session_arg,
            "--repo",
            "Schiste/Aethyme",
            "--",
        ];
        args.extend(command);
        let refusal = Invoke::new(args).cwd(tmp.path()).run();
        refusal.expect_code(1);
        refusal.assert_contains(expected);
    }

    let journal = Invoke::new(["broker", "operations", "--json"])
        .cwd(tmp.path())
        .run();
    journal.ok();
    assert!(journal.json().as_array().unwrap().is_empty());
}
