//! Verify-only repositories deliver through session branches and pull
//! requests, so the default branch -- not the integration branch -- is what a
//! session must merge with.
//!
//! Before this, `submit` simulated onto `aethyme/integration` even under
//! `[promote] mode = "verify-only"`, where nothing advances it except a
//! broker-mediated merge: after a pull request merged on the provider, a
//! session was verified against an old copy of the default branch. `push`
//! never compared the branch with the default branch at all, and `status`
//! had no per-session signal once a conflicting PR had merged.
//!
//! Every case runs against a local bare repository reached through a fake
//! `ssh` behind a GitHub-shaped `origin`, with a fake `gh` on `PATH`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

const FAKE_SSH: &str = r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE" ;;
  *git-receive-pack*) exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE" ;;
esac
exit 64
"#;

const FAKE_GH: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then printf '[]\n'; exit 0; fi
exit 64
"#;

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn git(cwd: &Path, args: &[&str]) {
    git_output(cwd, args);
}

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    /// A repository whose committed default-branch config sets `mode`, with
    /// the push lane enabled and an integration branch left at the initial
    /// commit, so it is stale as soon as the default branch moves.
    fn new(mode: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let bin = tmp.path().join("bin");
        for dir in [&repo, &remote, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (name, body) in [("ssh", FAKE_SSH), ("gh", FAKE_GH)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join(".gitignore"),
            ".aethyme/*\n!.aethyme/config.toml\n",
        )
        .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(repo.join("other.txt"), "other\n").unwrap();
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n\n[delivery]\npush_session_branches = true\n"),
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        git(&repo, &["branch", "aethyme/integration"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/project.git"],
        );
        git(
            &repo,
            &[
                "config",
                "core.sshCommand",
                bin.join("ssh").to_str().unwrap(),
            ],
        );
        let fixture = Self {
            tmp,
            repo,
            remote,
            bin,
        };
        fixture.git_env(&["push", "-qu", "origin", "main"]);
        fixture
    }

    fn git_env(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(self.tmp.path().join("host-state/operations.db"))
    }

    /// A live session with one committed change to `file`.
    fn session_changing(&self, file: &str, body: &str) -> (i64, PathBuf) {
        let session = self.broker().start_worktree("work", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join(file), body).unwrap();
        git(&worktree, &["add", file]);
        git(&worktree, &["commit", "-qm", "feat: session change"]);
        (session.id, worktree)
    }

    /// A pull request merging on the provider: the default branch moves on
    /// the remote and in the fetched `origin/main`, while the local `main`
    /// and the integration branch stay where they were.
    fn land_on_origin(&self, file: &str, body: &str) {
        let before = git_output(&self.repo, &["rev-parse", "HEAD"]);
        std::fs::write(self.repo.join(file), body).unwrap();
        git(&self.repo, &["add", file]);
        git(&self.repo, &["commit", "-qm", "merged elsewhere"]);
        self.git_env(&["push", "-q", "origin", "main"]);
        git(&self.repo, &["reset", "-q", "--hard", &before]);
    }

    fn run(&self, args: &[&str], remote: &Path) -> Output {
        Command::new(CLI)
            .args(args)
            .current_dir(&self.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_TEST_GIT_REMOTE", remote)
            .output()
            .unwrap()
    }

    fn push(&self, session: i64) -> serde_json::Value {
        let id = session.to_string();
        json(&self.run(&["push", "--session", &id, "--json"], &self.remote))
    }

    /// `status --json` with the remote made unreachable: status must not
    /// need it.
    fn status_offline(&self) -> serde_json::Value {
        json(&self.run(
            &["status", "--refresh", "--json"],
            &self.tmp.path().join("no-such-remote.git"),
        ))
    }
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

fn advice_rows<'a>(status: &'a serde_json::Value, id: &str) -> Vec<&'a serde_json::Value> {
    status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == id)
        .collect()
}

#[test]
fn verify_only_submit_verifies_against_the_default_branch_and_catches_its_conflict() {
    let fixture = Fixture::new("verify-only");
    let (session, _) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_origin("tracked.txt", "upstream\n");
    let integration = git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]);

    let outcome = fixture.broker().submit(session).unwrap();

    let base = outcome.verified_against.expect("verification base");
    assert_eq!(base.source, aethyme_broker::VERIFIED_AGAINST_UPSTREAM);
    assert_eq!(base.reference, "origin/main");
    assert_eq!(
        base.commit,
        git_output(&fixture.repo, &["rev-parse", "origin/main"])
    );
    assert_eq!(
        outcome.conflicts,
        vec!["tracked.txt".to_string()],
        "the change merged on the provider conflicts with this session"
    );
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        integration,
        "a verify-only submit leaves integration untouched"
    );
}

#[test]
fn a_promoting_repository_still_verifies_against_integration() {
    let fixture = Fixture::new("auto");
    let (session, _) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_origin("tracked.txt", "upstream\n");

    let outcome = fixture.broker().submit(session).unwrap();

    let base = outcome.verified_against.expect("verification base");
    assert_eq!(base.source, aethyme_broker::VERIFIED_AGAINST_INTEGRATION);
    assert!(
        outcome.conflicts.is_empty(),
        "integration does not hold the upstream change: {:?}",
        outcome.conflicts
    );
}

#[test]
fn push_reports_a_default_branch_change_that_would_conflict() {
    let fixture = Fixture::new("verify-only");
    let (session, _) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_origin("tracked.txt", "upstream\n");

    let report = fixture.push(session);

    let drift = &report["default_branch"];
    assert_eq!(drift["reference"], "origin/main", "{report}");
    assert_eq!(drift["behind"], 1, "{report}");
    assert_eq!(drift["ahead"], 1, "{report}");
    assert_eq!(drift["would_conflict"], true, "{report}");
    assert_eq!(
        drift["conflicting_paths"],
        serde_json::json!(["tracked.txt"])
    );
    assert_eq!(
        drift["suggested_command"], "git fetch origin && git merge origin/main",
        "a published branch catches up by merging: {report}"
    );
    assert!(report.get("default_branch_note").is_none(), "{report}");
}

#[test]
fn push_reports_a_clean_default_branch_change_as_clean() {
    let fixture = Fixture::new("verify-only");
    let (session, _) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_origin("other.txt", "upstream\n");

    let drift = &fixture.push(session)["default_branch"];
    assert_eq!(drift["behind"], 1, "{drift}");
    assert_eq!(drift["would_conflict"], false, "{drift}");
    assert_eq!(drift["conflicting_paths"], serde_json::json!([]));
}

#[test]
fn push_still_succeeds_when_the_default_branch_cannot_be_fetched() {
    let fixture = Fixture::new("verify-only");
    let (session, _) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_origin("tracked.txt", "upstream\n");
    // The remote loses its default branch: fetching it fails, pushing the
    // session branch still works.
    git(&fixture.remote, &["update-ref", "-d", "refs/heads/main"]);

    let report = fixture.push(session);

    assert!(report["pushed_oid"].is_string(), "{report}");
    let note = report["default_branch_note"].as_str().unwrap_or_default();
    assert!(note.contains("failed"), "{report}");
    assert_eq!(
        report["default_branch"]["would_conflict"], true,
        "the last fetched copy is still compared: {report}"
    );
}

#[test]
fn status_reports_the_session_behind_main_from_fetched_refs_only() {
    let fixture = Fixture::new("verify-only");
    let (conflicting, _) = fixture.session_changing("tracked.txt", "session\n");
    let (clean, _) = fixture.session_changing("other.txt", "session\n");
    fixture.land_on_origin("tracked.txt", "upstream\n");

    let status = fixture.status_offline();

    let rows = advice_rows(&status, "session.behind-main");
    let row = |session: i64| {
        rows.iter()
            .find(|row| row["session_id"] == session)
            .unwrap_or_else(|| panic!("no row for session {session}: {status}"))
    };
    assert_eq!(row(conflicting)["severity"], "warning", "{status}");
    assert_eq!(row(clean)["severity"], "info", "{status}");
    assert!(
        advice_rows(&status, "integration.behind-upstream").is_empty(),
        "integration is unused in a verify-only repository: {status}"
    );
}

/// Every status row that tells an agent integration may move, has drifted
/// from the default branch, or was bypassed by commits on it. Each group is
/// one condition; drift reports exactly one of its three ids.
const INTEGRATION_MOVEMENT_ADVICE: &[(&str, &[&str])] = &[
    (
        "live sessions may move integration",
        &["integration.may-move"],
    ),
    (
        "integration does not contain the default branch",
        &[
            "integration.upstream-main-ahead",
            "integration.fast-forward-available",
            "integration.stale-promotions",
        ],
    ),
    (
        "new sessions bypass a stale integration",
        &["integration.behind-upstream"],
    ),
    (
        "default-branch commits never passed through submit",
        &["main.external-writes"],
    ),
];

impl Fixture {
    /// Two live sessions, a pull request merged on the provider, a local
    /// default branch fast-forwarded to it, and an integration branch holding
    /// one promoted commit of its own: every integration-movement condition
    /// holds. The commit of its own matters -- a promoting broker
    /// fast-forwards an integration branch that is merely behind, which would
    /// clear the drift rows before `status` could report them.
    fn with_integration_left_behind(mode: &str) -> Self {
        let fixture = Self::new(mode);
        git(&fixture.repo, &["checkout", "-q", "aethyme/integration"]);
        std::fs::write(fixture.repo.join("promoted.txt"), "promoted\n").unwrap();
        git(&fixture.repo, &["add", "promoted.txt"]);
        git(&fixture.repo, &["commit", "-qm", "feat: promoted earlier"]);
        git(&fixture.repo, &["checkout", "-q", "main"]);
        fixture.session_changing("tracked.txt", "session\n");
        fixture.session_changing("other.txt", "session\n");
        fixture.land_on_origin("tracked.txt", "upstream\n");
        git(&fixture.repo, &["merge", "-q", "--ff-only", "origin/main"]);
        git(
            &fixture.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        fixture
    }
}

fn names_wait_stable(commands: &serde_json::Value) -> bool {
    commands
        .as_array()
        .unwrap()
        .iter()
        .any(|command| command.as_str().unwrap_or_default().contains("wait-stable"))
}

#[test]
fn verify_only_status_tells_no_session_that_integration_moves() {
    let fixture = Fixture::with_integration_left_behind("verify-only");

    let status = fixture.status_offline();

    for (condition, ids) in INTEGRATION_MOVEMENT_ADVICE {
        for id in *ids {
            assert!(
                advice_rows(&status, id).is_empty(),
                "{condition}: {id} in a verify-only repository: {status:#}"
            );
        }
    }
    let summary = &status["summary"];
    assert_eq!(summary["may_move_integration"], false, "{summary:#}");
    assert!(!names_wait_stable(&summary["commands"]), "{summary:#}");
    assert!(
        summary["message"]
            .as_str()
            .unwrap()
            .contains("verify-only: submit does not move integration"),
        "{summary:#}"
    );

    // Work promoted before the switch is still at risk, so that row stays --
    // without sending anyone to wait on integration.
    let unpublished = advice_rows(&status, "integration.unpublished-work");
    assert_eq!(unpublished.len(), 1, "{status:#}");
    assert!(
        !names_wait_stable(&unpublished[0]["commands"]),
        "{status:#}"
    );

    let doctor = json(&fixture.run(&["status", "doctor", "--json"], &fixture.remote));
    assert!(doctor.get("integration_movement").is_none(), "{doctor:#}");
}

/// The same repository promoting: every condition is real there, so every
/// row still appears. Without this the test above would pass on a fixture
/// that never raised any of them.
#[test]
fn a_promoting_repository_still_reports_integration_movement() {
    let fixture = Fixture::with_integration_left_behind("auto");

    let status = fixture.status_offline();

    for (condition, ids) in INTEGRATION_MOVEMENT_ADVICE {
        assert!(
            ids.iter().any(|id| !advice_rows(&status, id).is_empty()),
            "{condition}: none of {ids:?} in an auto repository: {status:#}"
        );
    }
    let summary = &status["summary"];
    assert_eq!(summary["may_move_integration"], true, "{summary:#}");
    assert!(names_wait_stable(&summary["commands"]), "{summary:#}");

    let doctor = json(&fixture.run(&["status", "doctor", "--json"], &fixture.remote));
    assert!(doctor["integration_movement"].is_object(), "{doctor:#}");
}
