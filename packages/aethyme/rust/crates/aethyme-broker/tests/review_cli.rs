#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{
    ExternalEventEnvelope, ExternalEventProvider, ExternalVerificationMethod,
    VerifiedExternalSource, external_event_digest,
};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct Fixture {
    root: tempfile::TempDir,
    remote: PathBuf,
    fake_bin: PathBuf,
    writes: PathBuf,
    host_state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::new_with_policy(
            "[review]\nenabled = true\nrequired_approvals = 1\nunlock_adapter = \"github_label\"\nunlock_label = \"validation-ready\"\n\n[publication]\nmode = \"review_gated\"\nallow_break_glass = true\n",
        )
    }

    fn new_with_policy(policy: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
        std::fs::write(root.path().join(".aethyme/config.toml"), policy).unwrap();
        std::fs::write(
            root.path().join(".gitignore"),
            "/.aethyme/broker.db*\n/remote.git/\n/fake-bin/\n/gh-writes\n/host-state/\n",
        )
        .unwrap();
        std::fs::write(root.path().join("README.md"), "initial\n").unwrap();
        git(root.path(), &["add", "-A"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        git(
            root.path(),
            &["remote", "add", "origin", "git@github.com:acme/product.git"],
        );

        let remote = root.path().join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(
            root.path(),
            &["push", "-q", remote.to_str().unwrap(), "main:main"],
        );
        let initial = git(root.path(), &["rev-parse", "main"]);
        git(
            root.path(),
            &["update-ref", "refs/remotes/origin/main", &initial],
        );

        let fake_bin = root.path().join("fake-bin");
        std::fs::create_dir(&fake_bin).unwrap();
        let gh = fake_bin.join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  if [ "$AETHYME_FAKE_GH_MODE" = "outage" ]; then
    exit 1
  fi
  printf '{"number":42,"baseRefName":"%s","headRefOid":"%s","state":"%s","isDraft":%s,"reviewDecision":%s}\n' \
    "${AETHYME_REVIEW_BASE:-main}" "$AETHYME_REVIEW_HEAD" "${AETHYME_REVIEW_STATE:-OPEN}" \
    "${AETHYME_REVIEW_DRAFT:-true}" "${AETHYME_REVIEW_DECISION:-null}"
  exit 0
fi
if [ "$1" = "api" ]; then
  if [ "$AETHYME_FAKE_GH_MODE" = "check-outage" ]; then
    exit 1
  fi
  printf '%s\n' "$AETHYME_CHECK_RUNS"
  exit 0
fi
printf '%s %s\n' "$1" "$2" >> "$AETHYME_REVIEW_WRITES"
if [ "$AETHYME_FAKE_GH_MODE" = "write-fail" ]; then
  exit 1
fi
exit 0
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).unwrap();
        let ssh = fake_bin.join("ssh");
        std::fs::write(
            &ssh,
            r#"#!/bin/sh
case "$*" in
  *git-upload-pack*)
    exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE"
    ;;
  *git-receive-pack*)
    exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE"
    ;;
esac
exit 64
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ssh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ssh, permissions).unwrap();
        git(
            root.path(),
            &["config", "core.sshCommand", ssh.to_str().unwrap()],
        );
        let writes = root.path().join("gh-writes");
        let host_state = root.path().join("host-state");
        Self {
            root,
            remote,
            fake_bin,
            writes,
            host_state,
        }
    }

    fn run(&self, args: &[&str], head: &str, draft: bool, decision: Option<&str>) -> Output {
        self.run_with_evidence(args, head, "main", "OPEN", draft, decision, None)
    }

    fn run_with_evidence(
        &self,
        args: &[&str],
        head: &str,
        base: &str,
        state: &str,
        draft: bool,
        decision: Option<&str>,
        mode: Option<&str>,
    ) -> Output {
        self.command_with_evidence(args, head, base, state, draft, decision, mode)
            .output()
            .unwrap()
    }

    fn run_with_check(
        &self,
        args: &[&str],
        head: &str,
        draft: bool,
        check_runs: &str,
        mode: Option<&str>,
    ) -> Output {
        self.command_with_evidence(args, head, "main", "OPEN", draft, None, mode)
            .env("AETHYME_CHECK_RUNS", check_runs)
            .output()
            .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn command_with_evidence(
        &self,
        args: &[&str],
        head: &str,
        base: &str,
        state: &str,
        draft: bool,
        decision: Option<&str>,
        mode: Option<&str>,
    ) -> Command {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let decision = decision
            .map(|value| format!("\"{value}\""))
            .unwrap_or_else(|| "null".into());
        let mut command = Command::new(CLI);
        command
            .args(args)
            .current_dir(self.root.path())
            .env("PATH", path)
            .env("AETHYME_HOST_STATE_DIR", &self.host_state)
            .env("AETHYME_REVIEW_WRITES", &self.writes)
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .env("AETHYME_REVIEW_HEAD", head)
            .env("AETHYME_REVIEW_BASE", base)
            .env("AETHYME_REVIEW_STATE", state)
            .env("AETHYME_REVIEW_DRAFT", draft.to_string())
            .env("AETHYME_REVIEW_DECISION", decision);
        if let Some(mode) = mode {
            command.env("AETHYME_FAKE_GH_MODE", mode);
        }
        command
    }

    fn writes(&self) -> String {
        std::fs::read_to_string(&self.writes).unwrap_or_default()
    }

    fn remote_main(&self) -> String {
        git(&self.remote, &["rev-parse", "refs/heads/main"])
    }
}

fn envelope(id: &str, event_type: &str, commit: &str) -> ExternalEventEnvelope {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut envelope = ExternalEventEnvelope {
        schema_version: 1,
        provider: ExternalEventProvider::Github,
        provider_event_id: id.into(),
        event_type: event_type.into(),
        repository: "github.com/acme/product".into(),
        target_branch: "main".into(),
        pr_number: 42,
        commit_sha: commit.into(),
        occurred_at: now,
        verified_source: VerifiedExternalSource {
            method: ExternalVerificationMethod::AuthenticatedPoll,
            verified_at: now,
        },
        normalized_digest: String::new(),
    };
    envelope.normalized_digest = external_event_digest(&envelope);
    envelope
}

fn check_run_evidence(head: &str, app_slug: &str, conclusion: &str) -> String {
    serde_json::json!({
        "total_count": 1,
        "check_runs": [{
            "id": 91,
            "name": "review-gate/codex",
            "head_sha": head,
            "status": "completed",
            "conclusion": conclusion,
            "app": {"slug": app_slug}
        }]
    })
    .to_string()
}

fn promoted_review_candidate(fixture: &Fixture) -> (String, String, i64) {
    let start = fixture.run(
        &["start", "--task", "reviewed ship", "--json"],
        "",
        true,
        None,
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    std::fs::write(worktree.join("README.md"), "reviewed candidate\n").unwrap();
    git(&worktree, &["add", "README.md"]);
    git(&worktree, &["commit", "-qm", "reviewed candidate"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    let register = fixture.run(
        &[
            "review",
            "register",
            "--session",
            &session_id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        register.status.success(),
        "{}",
        String::from_utf8_lossy(&register.stderr)
    );
    let submit = fixture.run(
        &["submit", "--session", &session_id, "--json"],
        &head,
        true,
        None,
    );
    assert!(
        submit.status.success(),
        "{}",
        String::from_utf8_lossy(&submit.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&submit.stdout).unwrap();
    let entry_id = outcome["entry"]["id"].as_i64().unwrap();
    (session_id, head, entry_id)
}

fn unlock_review(fixture: &Fixture, session_id: &str, head: &str) {
    let request = fixture.run(
        &["review", "request", "--session", session_id, "--json"],
        head,
        true,
        None,
    );
    assert!(
        request.status.success(),
        "{}",
        String::from_utf8_lossy(&request.stderr)
    );
    let event_path = fixture.root.path().join("ship-approved.json");
    std::fs::write(
        &event_path,
        serde_json::to_vec(&envelope("ship-approval", "review_approved", head)).unwrap(),
    )
    .unwrap();
    let ingest = fixture.run(
        &[
            "external-events",
            "ingest",
            event_path.to_str().unwrap(),
            "--json",
        ],
        head,
        false,
        Some("APPROVED"),
    );
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let unlock = fixture.run(
        &["review", "unlock", "--session", session_id, "--json"],
        head,
        false,
        Some("APPROVED"),
    );
    assert!(
        unlock.status.success(),
        "{}",
        String::from_utf8_lossy(&unlock.stderr)
    );
}

#[test]
fn review_gated_ship_requires_every_entry_and_revalidates_live_evidence() {
    let fixture = Fixture::new();
    let (session_id, head, entry_id) = promoted_review_candidate(&fixture);
    let entry = entry_id.to_string();
    let remote_before = fixture.remote_main();

    let missing = fixture.run(
        &["ship", "plan", "--entry", &entry, "--json"],
        &head,
        true,
        None,
    );
    assert!(
        missing.status.success(),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let missing_plan: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(
        missing_plan["publication_policy"]["policy"]["mode"],
        "review_gated"
    );
    assert_eq!(missing_plan["publication_policy"]["satisfied"], false);
    assert_eq!(
        missing_plan["publication_policy"]["evidence"][0]["covered"],
        false
    );
    let publication_sha = missing_plan["publication_sha"]
        .as_str()
        .unwrap()
        .to_string();
    let config_path = fixture.root.path().join(".aethyme/config.toml");
    let committed_config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        &config_path,
        committed_config.replace("mode = \"review_gated\"", "mode = \"direct\""),
    )
    .unwrap();
    let dirty_bypass = fixture.run(
        &["ship", "plan", "--entry", &entry, "--json"],
        &head,
        true,
        None,
    );
    assert!(dirty_bypass.status.success());
    let dirty_plan: serde_json::Value = serde_json::from_slice(&dirty_bypass.stdout).unwrap();
    assert_eq!(
        dirty_plan["publication_policy"]["policy"]["mode"], "review_gated",
        "an uncommitted checkout edit must not weaken the promoted policy"
    );
    std::fs::write(&config_path, committed_config).unwrap();
    let refused = fixture.run(
        &[
            "ship",
            "execute",
            "--entry",
            &entry,
            "--confirm",
            &publication_sha,
        ],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not covered by review evidence"));
    assert_eq!(fixture.remote_main(), remote_before);

    unlock_review(&fixture, &session_id, &head);
    let planned = fixture.run(
        &["ship", "plan", "--entry", &entry, "--json"],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["publication_policy"]["satisfied"], true);
    assert_eq!(plan["publication_policy"]["source_commit"], publication_sha);
    assert_eq!(
        plan["publication_policy"]["evidence"][0]["queue_entry_id"],
        entry_id
    );
    assert_eq!(
        plan["publication_policy"]["evidence"][0]["reviewed_commit_sha"],
        head
    );

    for (base, state, live_head, decision, mode, expected) in [
        (
            "release",
            "OPEN",
            head.as_str(),
            "APPROVED",
            None,
            "no longer matches",
        ),
        (
            "main",
            "CLOSED",
            head.as_str(),
            "APPROVED",
            None,
            "no longer matches",
        ),
        (
            "main",
            "OPEN",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "APPROVED",
            None,
            "no longer matches",
        ),
        (
            "main",
            "OPEN",
            head.as_str(),
            "CHANGES_REQUESTED",
            None,
            "no longer matches",
        ),
        (
            "main",
            "OPEN",
            head.as_str(),
            "APPROVED",
            Some("outage"),
            "review lifecycle",
        ),
    ] {
        let drifted = fixture.run_with_evidence(
            &[
                "ship",
                "execute",
                "--entry",
                &entry,
                "--confirm",
                &publication_sha,
            ],
            live_head,
            base,
            state,
            false,
            Some(decision),
            mode,
        );
        assert!(!drifted.status.success());
        assert!(
            String::from_utf8_lossy(&drifted.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&drifted.stderr)
        );
        assert_eq!(fixture.remote_main(), remote_before);
    }

    let shipped = fixture.run(
        &[
            "ship",
            "execute",
            "--entry",
            &entry,
            "--confirm",
            &publication_sha,
            "--json",
        ],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(
        shipped.status.success(),
        "{}",
        String::from_utf8_lossy(&shipped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&shipped.stdout).unwrap();
    assert_eq!(report["publication_authorization"]["kind"], "reviewed");
    assert_eq!(
        report["publication_authorization"]["live_evidence_revalidated"],
        true
    );
    assert_eq!(fixture.remote_main(), publication_sha);
}

#[test]
fn break_glass_requires_committed_opt_in_and_journals_only_a_reason_digest() {
    let fixture = Fixture::new();
    let (_, head, entry_id) = promoted_review_candidate(&fixture);
    let entry = entry_id.to_string();
    let plan = fixture.run(
        &["ship", "plan", "--entry", &entry, "--json"],
        &head,
        true,
        None,
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let publication_sha = plan["publication_sha"].as_str().unwrap().to_string();
    let secret_reason = "incident-authority-token-should-never-be-stored";
    let shipped = fixture.run(
        &[
            "ship",
            "execute",
            "--entry",
            &entry,
            "--confirm",
            &publication_sha,
            "--break-glass",
            "--reason",
            secret_reason,
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        shipped.status.success(),
        "{}",
        String::from_utf8_lossy(&shipped.stderr)
    );
    let serialized = String::from_utf8(shipped.stdout).unwrap();
    assert!(!serialized.contains(secret_reason));
    let report: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(report["publication_authorization"]["kind"], "break_glass");
    assert_eq!(
        report["publication_authorization"]["reason_digest"],
        aethyme_broker::sha256_bytes(secret_reason.as_bytes())
    );
    let operations = fixture.run(&["operations", "list", "--json"], "", true, None);
    assert!(operations.status.success());
    assert!(!String::from_utf8_lossy(&operations.stdout).contains(secret_reason));
    assert_eq!(fixture.remote_main(), publication_sha);
}

#[test]
fn review_cli_requires_submission_revalidates_evidence_and_unlocks_once() {
    let fixture = Fixture::new();
    let adopt = fixture.run(&["adopt", "--task", "review", "--json"], "", true, None);
    assert!(
        adopt.status.success(),
        "{}",
        String::from_utf8_lossy(&adopt.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&adopt.stdout).unwrap();
    let session_id = session["id"].as_i64().unwrap();

    std::fs::write(fixture.root.path().join("README.md"), "candidate\n").unwrap();
    git(fixture.root.path(), &["add", "README.md"]);
    git(fixture.root.path(), &["commit", "-qm", "candidate"]);
    let head = git(fixture.root.path(), &["rev-parse", "HEAD"]);
    let id = session_id.to_string();

    let register = fixture.run(
        &[
            "review",
            "register",
            "--session",
            &id,
            "--repo",
            "Acme/Product",
            "--pr",
            "42",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        register.status.success(),
        "{}",
        String::from_utf8_lossy(&register.stderr)
    );
    let registered: serde_json::Value = serde_json::from_slice(&register.stdout).unwrap();
    assert_eq!(registered["lifecycle"]["state"], "draft_opened");
    assert_eq!(registered["lifecycle"]["commit_sha"], head);

    let premature = fixture.run(
        &["review", "request", "--session", &id, "--json"],
        &head,
        true,
        None,
    );
    assert!(!premature.status.success());
    assert!(String::from_utf8_lossy(&premature.stderr).contains("successful broker submission"));
    assert!(fixture.writes().is_empty());

    let submit = fixture.run(&["submit", "--session", &id, "--json"], &head, true, None);
    assert!(
        submit.status.success(),
        "{}",
        String::from_utf8_lossy(&submit.stderr)
    );
    let requested = fixture.run(
        &["review", "request", "--session", &id, "--json"],
        &head,
        true,
        None,
    );
    assert!(
        requested.status.success(),
        "{}",
        String::from_utf8_lossy(&requested.stderr)
    );
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr ready")
            .count(),
        1
    );

    let drift = fixture.run(
        &["review", "request", "--session", &id, "--json"],
        &"a".repeat(40),
        false,
        None,
    );
    assert!(!drift.status.success());
    assert!(String::from_utf8_lossy(&drift.stderr).contains("head drifted"));
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr ready")
            .count(),
        1
    );

    let event_path = fixture.root.path().join("approved.json");
    std::fs::write(
        &event_path,
        serde_json::to_vec(&envelope("approval-1", "review_approved", &head)).unwrap(),
    )
    .unwrap();
    let ingest = fixture.run(
        &[
            "external-events",
            "ingest",
            event_path.to_str().unwrap(),
            "--json",
        ],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );

    let unlock = fixture.run(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(
        unlock.status.success(),
        "{}",
        String::from_utf8_lossy(&unlock.stderr)
    );
    let unlocked: serde_json::Value = serde_json::from_slice(&unlock.stdout).unwrap();
    assert_eq!(unlocked["lifecycle"]["state"], "validation_unlocked");
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr edit")
            .count(),
        1
    );

    let repeated = fixture.run(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        Some("APPROVED"),
    );
    assert!(repeated.status.success());
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr edit")
            .count(),
        1
    );

    let show = fixture.run(
        &["review", "show", "--session", &id, "--json"],
        "",
        false,
        None,
    );
    assert!(show.status.success());
    assert!(!fixture.root.path().join(".aethyme/broker.sock").exists());
    let serialized = String::from_utf8(show.stdout).unwrap();
    for forbidden in [
        "credential",
        "review body",
        "task text",
        fixture.root.path().to_str().unwrap(),
    ] {
        assert!(!serialized.contains(forbidden), "show leaked {forbidden:?}");
    }
}

#[test]
fn review_cli_default_profile_and_provider_outage_do_not_mutate_state() {
    let fixture = Fixture::new();
    let adopt = fixture.run(&["adopt", "--task", "review", "--json"], "", true, None);
    let session: serde_json::Value = serde_json::from_slice(&adopt.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let head = git(fixture.root.path(), &["rev-parse", "HEAD"]);
    let path = format!(
        "{}:{}",
        fixture.fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let outage = Command::new(CLI)
        .args([
            "review",
            "register",
            "--session",
            &id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
        ])
        .current_dir(fixture.root.path())
        .env("PATH", path)
        .env("AETHYME_HOST_STATE_DIR", &fixture.host_state)
        .env("AETHYME_REVIEW_HEAD", &head)
        .env("AETHYME_REVIEW_WRITES", &fixture.writes)
        .env("AETHYME_FAKE_GH_MODE", "outage")
        .output()
        .unwrap();
    assert!(!outage.status.success());
    let show = fixture.run(&["review", "show", "--session", &id], "", true, None);
    assert!(!show.status.success());
    assert!(String::from_utf8_lossy(&show.stderr).contains("no review lifecycle"));
    assert!(fixture.writes().is_empty());
}

#[test]
fn trusted_check_run_can_satisfy_review_without_an_approval_webhook() {
    let fixture = Fixture::new_with_policy(
        "[review]\nenabled = true\nevidence_adapter = \"github_check_run\"\nrequired_approvals = 0\nevidence_check_name = \"review-gate/codex\"\nevidence_app_slug = \"github-actions\"\nunlock_adapter = \"github_label\"\nunlock_label = \"validation-ready\"\n\n[publication]\nmode = \"review_gated\"\nallow_break_glass = true\n",
    );
    let adopt = fixture.run(&["adopt", "--task", "bot review", "--json"], "", true, None);
    assert!(adopt.status.success());
    let session: serde_json::Value = serde_json::from_slice(&adopt.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(fixture.root.path().join("README.md"), "candidate\n").unwrap();
    git(fixture.root.path(), &["add", "README.md"]);
    git(fixture.root.path(), &["commit", "-qm", "candidate"]);
    let head = git(fixture.root.path(), &["rev-parse", "HEAD"]);
    let empty = r#"{"total_count":0,"check_runs":[]}"#;

    for args in [
        vec![
            "review",
            "register",
            "--session",
            &id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
            "--json",
        ],
        vec!["submit", "--session", &id, "--json"],
        vec!["review", "request", "--session", &id, "--json"],
    ] {
        let output = fixture.run_with_check(&args, &head, true, empty, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let approval_path = fixture.root.path().join("formal-approval.json");
    std::fs::write(
        &approval_path,
        serde_json::to_vec(&envelope("ignored-approval", "review_approved", &head)).unwrap(),
    )
    .unwrap();
    let ignored = fixture.run_with_check(
        &[
            "external-events",
            "ingest",
            approval_path.to_str().unwrap(),
            "--json",
        ],
        &head,
        false,
        empty,
        None,
    );
    assert!(ignored.status.success());
    let shown = fixture.run(
        &["review", "show", "--session", &id, "--json"],
        "",
        false,
        None,
    );
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["lifecycle"]["state"], "review_requested");

    let wrong_actor = check_run_evidence(&head, "untrusted", "success");
    let refused = fixture.run_with_check(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        &wrong_actor,
        None,
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("exact pull request head"));

    let truncated = r#"{"total_count":101,"check_runs":[]}"#;
    let refused = fixture.run_with_check(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        truncated,
        None,
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("truncated"));

    let satisfied = check_run_evidence(&head, "github-actions", "success");
    let unlocked = fixture.run_with_check(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        &satisfied,
        None,
    );
    assert!(
        unlocked.status.success(),
        "{}",
        String::from_utf8_lossy(&unlocked.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&unlocked.stdout).unwrap();
    assert_eq!(report["lifecycle"]["state"], "validation_unlocked");
    assert_eq!(report["policy"]["evidence_adapter"], "github_check_run");
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr edit")
            .count(),
        1
    );

    let repeated = fixture.run_with_check(
        &["review", "unlock", "--session", &id, "--json"],
        &head,
        false,
        &satisfied,
        None,
    );
    assert!(repeated.status.success());
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr edit")
            .count(),
        1
    );
}

#[test]
fn unknown_ready_outcome_preserves_state_and_blocks_blind_retry() {
    let fixture = Fixture::new();
    let adopt = fixture.run(&["adopt", "--task", "review", "--json"], "", true, None);
    let session: serde_json::Value = serde_json::from_slice(&adopt.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(fixture.root.path().join("README.md"), "candidate\n").unwrap();
    git(fixture.root.path(), &["add", "README.md"]);
    git(fixture.root.path(), &["commit", "-qm", "candidate"]);
    let head = git(fixture.root.path(), &["rev-parse", "HEAD"]);
    assert!(
        fixture
            .run(
                &[
                    "review",
                    "register",
                    "--session",
                    &id,
                    "--repo",
                    "acme/product",
                    "--pr",
                    "42",
                ],
                &head,
                true,
                None,
            )
            .status
            .success()
    );
    assert!(
        fixture
            .run(&["submit", "--session", &id, "--json"], &head, true, None)
            .status
            .success()
    );

    let path = format!(
        "{}:{}",
        fixture.fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let failed = Command::new(CLI)
        .args(["review", "request", "--session", &id])
        .current_dir(fixture.root.path())
        .env("PATH", path)
        .env("AETHYME_HOST_STATE_DIR", &fixture.host_state)
        .env("AETHYME_REVIEW_WRITES", &fixture.writes)
        .env("AETHYME_REVIEW_HEAD", &head)
        .env("AETHYME_REVIEW_DRAFT", "true")
        .env("AETHYME_REVIEW_DECISION", "null")
        .env("AETHYME_FAKE_GH_MODE", "write-fail")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let shown = fixture.run(
        &["review", "show", "--session", &id, "--json"],
        "",
        false,
        None,
    );
    let state: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(state["lifecycle"]["state"], "local_submission_verified");

    let retry = fixture.run(&["review", "request", "--session", &id], &head, true, None);
    assert!(!retry.status.success());
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("write-blocked"),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        fixture
            .writes()
            .lines()
            .filter(|line| *line == "pr ready")
            .count(),
        1
    );
}

#[test]
fn closed_review_lifecycle_can_be_reassigned_or_abandoned_without_losing_audit_history() {
    let fixture = Fixture::new();
    let head = git(fixture.root.path(), &["rev-parse", "HEAD"]);

    let start = fixture.run(
        &["start", "--task", "original review owner", "--json"],
        &head,
        true,
        None,
    );
    assert!(start.status.success());
    let original: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    let original_id = original["id"].as_i64().unwrap().to_string();
    let register = fixture.run(
        &[
            "review",
            "register",
            "--session",
            &original_id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        register.status.success(),
        "{}",
        String::from_utf8_lossy(&register.stderr)
    );
    let close = fixture.run(
        &["close", "--session", &original_id, "--json"],
        &head,
        true,
        None,
    );
    assert!(close.status.success());

    let replacement = fixture.run(
        &["start", "--task", "replacement review owner", "--json"],
        &head,
        true,
        None,
    );
    assert!(replacement.status.success());
    let replacement: serde_json::Value = serde_json::from_slice(&replacement.stdout).unwrap();
    let replacement_id = replacement["id"].as_i64().unwrap().to_string();

    let duplicate = fixture.run(
        &[
            "review",
            "register",
            "--session",
            &replacement_id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(!duplicate.status.success());
    let duplicate_error = String::from_utf8_lossy(&duplicate.stderr);
    assert!(
        duplicate_error.contains("already owned by session"),
        "{duplicate_error}"
    );
    assert!(
        !duplicate_error.contains("UNIQUE constraint"),
        "{duplicate_error}"
    );

    let reassign = fixture.run(
        &[
            "review",
            "reassign",
            "--session",
            &original_id,
            "--to-session",
            &replacement_id,
            "--reason",
            "continue the same reviewed commit",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        reassign.status.success(),
        "{}",
        String::from_utf8_lossy(&reassign.stderr)
    );
    let reassigned: serde_json::Value = serde_json::from_slice(&reassign.stdout).unwrap();
    assert_eq!(
        reassigned["lifecycle"]["session_id"],
        replacement_id.parse::<i64>().unwrap()
    );
    assert_eq!(reassigned["lifecycle"]["active"], true);
    assert_eq!(reassigned["lifecycle"]["generation"], 1);

    let abandon = fixture.run(
        &[
            "review",
            "abandon",
            "--session",
            &replacement_id,
            "--reason",
            "restart review coordination under a fresh session",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        abandon.status.success(),
        "{}",
        String::from_utf8_lossy(&abandon.stderr)
    );
    let abandoned: serde_json::Value = serde_json::from_slice(&abandon.stdout).unwrap();
    assert_eq!(abandoned["abandoned"], true);
    assert_eq!(abandoned["lifecycle"]["active"], false);
    assert_eq!(
        abandoned["lifecycle"]["abandon_reason_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let fresh = fixture.run(
        &["start", "--task", "fresh review registration", "--json"],
        &head,
        true,
        None,
    );
    assert!(fresh.status.success());
    let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout).unwrap();
    let fresh_id = fresh["id"].as_i64().unwrap().to_string();
    let fresh_register = fixture.run(
        &[
            "review",
            "register",
            "--session",
            &fresh_id,
            "--repo",
            "acme/product",
            "--pr",
            "42",
            "--json",
        ],
        &head,
        true,
        None,
    );
    assert!(
        fresh_register.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh_register.stderr)
    );

    let events = fixture.run(
        &["events", "--kind", "review.lifecycle", "--json"],
        &head,
        true,
        None,
    );
    let events = String::from_utf8(events.stdout).unwrap();
    assert!(events.contains("review.lifecycle_reassigned"));
    assert!(events.contains("review.lifecycle_abandoned"));
    assert!(!events.contains("continue the same reviewed commit"));
    assert!(!events.contains("restart review coordination"));
}
