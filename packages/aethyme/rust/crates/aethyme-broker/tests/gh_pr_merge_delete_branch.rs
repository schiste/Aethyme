use aethyme_broker::{Broker, CoordinatedCommand, OperationProvider, OperationStatus};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
struct RestoreEnv {
    name: &'static str,
    previous: Option<OsString>,
}

#[cfg(unix)]
impl RestoreEnv {
    fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(name);
        // SAFETY: This binary has a single test; it changes the environment on the test thread, then launches child processes after each change. No other test or thread reads the environment concurrently.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }
}

#[cfg(unix)]
impl Drop for RestoreEnv {
    fn drop(&mut self) {
        // SAFETY: This binary has a single test; it changes the environment on the test thread, then launches child processes after each change. No other test or thread reads the environment concurrently.
        unsafe {
            if let Some(value) = &self.previous {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }
}

#[cfg(unix)]
fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "init"]);
}

#[cfg(unix)]
fn worktree(repo: &Path, parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

#[cfg(unix)]
fn merge_request(session_id: i64, tail: &[&str]) -> CoordinatedCommand {
    let mut args = vec!["pr".to_string(), "merge".to_string()];
    args.extend(tail.iter().map(|arg| (*arg).to_string()));
    CoordinatedCommand {
        session_id,
        provider: OperationProvider::Github,
        repository: Some("owner/repo".into()),
        resolved_target: None,
        scope: Some("github:test".into()),
        declared_effect: None,
        destructive_confirmed: true,
        cross_session: None,
        authorization_reason: Some("test operator authorization".into()),
        args,
    }
}

#[cfg(unix)]
fn close_request(session_id: i64, tail: &[&str]) -> CoordinatedCommand {
    let mut request = merge_request(session_id, tail);
    request.args[1] = "close".into();
    request
}

#[cfg(unix)]
fn clear_logs(log: &Path, mutations: &Path) {
    std::fs::write(log, "").unwrap();
    std::fs::write(mutations, "").unwrap();
}

#[cfg(unix)]
fn contents(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[cfg(unix)]
#[test]
fn gh_pr_delete_branch_preflights_and_protects_live_session_heads() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let worktrees = root.join("worktrees");
    std::fs::create_dir(&worktrees).unwrap();
    let own_worktree = worktree(&repo, &worktrees, "own");
    let other_worktree = worktree(&repo, &worktrees, "other");

    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = root.join("gh-log");
    let mutations = root.join("gh-mutations");
    let response = root.join("gh-view.json");
    let fake_gh = bin.join("gh");
    let script = r#"#!/bin/sh
printf 'repo=%s args=%s\n' "$GH_REPO" "$*" >> "$AETHYME_FAKE_GH_LOG"
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  cat "$AETHYME_FAKE_GH_VIEW"
  exit "$AETHYME_FAKE_GH_VIEW_RC"
fi
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_MUTATIONS"
exit 0
"#;
    std::fs::write(&fake_gh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&fake_gh).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_gh, permissions).unwrap();
    clear_logs(&log, &mutations);
    std::fs::write(&response, "{}").unwrap();

    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin.display(), original_path.to_string_lossy());
    let _path = RestoreEnv::set("PATH", path);
    let _log = RestoreEnv::set("AETHYME_FAKE_GH_LOG", &log);
    let _mutations = RestoreEnv::set("AETHYME_FAKE_GH_MUTATIONS", &mutations);
    let _response = RestoreEnv::set("AETHYME_FAKE_GH_VIEW", &response);
    let _view_rc = RestoreEnv::set("AETHYME_FAKE_GH_VIEW_RC", "0");

    let mut broker = Broker::open(&repo).unwrap();
    let own = broker.adopt(&own_worktree, None).unwrap();
    let other = broker
        .adopt(&other_worktree, Some("other session owner"))
        .unwrap();

    std::fs::write(
        &response,
        r#"{"headRefName":"agent/other","headRepository":{"nameWithOwner":"owner/repo"}}"#,
    )
    .unwrap();
    let error = broker
        .run_coordinated_operation(merge_request(own.id, &["42", "--delete-branch"]))
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&format!("belongs to live session {}", other.id)),
        "{message}"
    );
    assert!(
        contents(&log)
            .contains("repo=owner/repo args=pr view 42 --json headRefName,headRepository"),
        "{}",
        contents(&log)
    );
    assert!(contents(&mutations).is_empty());
    assert!(broker.store().coordinated_operations().unwrap().is_empty());

    clear_logs(&log, &mutations);
    let unknown_flag = broker
        .run_coordinated_operation(merge_request(
            own.id,
            &["--unknown-flag", "42", "--delete-branch"],
        ))
        .unwrap_err();
    assert!(unknown_flag.to_string().contains("unrecognized flag"));
    assert!(contents(&log).is_empty());
    assert!(contents(&mutations).is_empty());
    assert!(broker.store().coordinated_operations().unwrap().is_empty());

    clear_logs(&log, &mutations);
    std::fs::write(&response, "{}").unwrap();
    let incomplete_view = broker
        .run_coordinated_operation(merge_request(own.id, &["42", "--delete-branch"]))
        .unwrap_err();
    assert!(
        incomplete_view
            .to_string()
            .contains("no branch-deleting command was sent")
    );
    assert!(contents(&log).contains("pr view 42"));
    assert!(contents(&mutations).is_empty());
    assert!(broker.store().coordinated_operations().unwrap().is_empty());

    clear_logs(&log, &mutations);
    std::fs::write(
        &response,
        r#"{"headRefName":"agent/other","headRepository":{"nameWithOwner":"owner/repo"}}"#,
    )
    .unwrap();
    // SAFETY: This binary has a single test and no other thread reads the environment concurrently.
    unsafe { std::env::set_var("AETHYME_FAKE_GH_VIEW_RC", "1") };
    let failed_view = broker
        .run_coordinated_operation(merge_request(own.id, &["42", "--delete-branch"]))
        .unwrap_err();
    assert!(
        failed_view
            .to_string()
            .contains("read-only PR lookup failed")
    );
    assert!(contents(&mutations).is_empty());
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
    // SAFETY: This binary has a single test and no other thread reads the environment concurrently.
    unsafe { std::env::set_var("AETHYME_FAKE_GH_VIEW_RC", "0") };

    clear_logs(&log, &mutations);
    std::fs::write(
        &response,
        r#"{"headRefName":"agent/other","headRepository":{"nameWithOwner":"fork/repo"}}"#,
    )
    .unwrap();
    let fork_head = broker
        .run_coordinated_operation(merge_request(own.id, &["42", "--delete-branch"]))
        .unwrap();
    assert_eq!(fork_head.operation.status, OperationStatus::Succeeded);
    assert!(contents(&mutations).contains("pr merge 42 --delete-branch"));

    clear_logs(&log, &mutations);
    std::fs::write(
        &response,
        r#"{"headRefName":"agent/other","headRepository":{"nameWithOwner":"owner/repo"}}"#,
    )
    .unwrap();
    let operations_before_close = broker.store().coordinated_operations().unwrap().len();
    let close_head = broker
        .run_coordinated_operation(close_request(own.id, &["42", "--delete-branch"]))
        .unwrap_err();
    assert!(close_head.to_string().contains("belongs to live session"));
    assert!(contents(&log).contains("pr view 42 --json headRefName,headRepository"));
    assert!(contents(&mutations).is_empty());
    assert_eq!(
        broker.store().coordinated_operations().unwrap().len(),
        operations_before_close
    );

    clear_logs(&log, &mutations);
    std::fs::write(
        &response,
        r#"{"headRefName":"agent/other","headRepository":{"nameWithOwner":"owner/repo"}}"#,
    )
    .unwrap();
    let no_selector = broker
        .run_coordinated_operation(merge_request(own.id, &["--delete-branch"]))
        .unwrap_err();
    assert!(no_selector.to_string().contains("belongs to live session"));
    assert!(contents(&log).contains("pr view --json headRefName,headRepository"));
    assert!(contents(&mutations).is_empty());

    clear_logs(&log, &mutations);
    let mut authorized = merge_request(own.id, &["42", "--delete-branch"]);
    authorized.cross_session = Some(other.id);
    let report = broker.run_coordinated_operation(authorized).unwrap();
    assert_eq!(report.operation.status, OperationStatus::Succeeded);
    assert!(contents(&mutations).contains("pr merge 42 --delete-branch"));
}

#[cfg(not(unix))]
#[test]
fn gh_pr_delete_branch_preflights_and_protects_live_session_heads() {
    // The executable-based fake GitHub CLI in this regression test is Unix-only.
}
