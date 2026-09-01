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
    fake_bin: PathBuf,
    writes: PathBuf,
    host_state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
        std::fs::write(
            root.path().join(".aethyme/config.toml"),
            "[review]\nenabled = true\nrequired_approvals = 1\nunlock_adapter = \"github_label\"\nunlock_label = \"validation-ready\"\n",
        )
        .unwrap();
        std::fs::write(root.path().join(".gitignore"), "/.aethyme/broker.db*\n").unwrap();
        std::fs::write(root.path().join("README.md"), "initial\n").unwrap();
        git(root.path(), &["add", "-A"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        git(
            root.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/product.git",
            ],
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
        let writes = root.path().join("gh-writes");
        let host_state = root.path().join("host-state");
        Self {
            root,
            fake_bin,
            writes,
            host_state,
        }
    }

    fn run(&self, args: &[&str], head: &str, draft: bool, decision: Option<&str>) -> Output {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let decision = decision
            .map(|value| format!("\"{value}\""))
            .unwrap_or_else(|| "null".into());
        Command::new(CLI)
            .args(args)
            .current_dir(self.root.path())
            .env("PATH", path)
            .env("AETHYME_HOST_STATE_DIR", &self.host_state)
            .env("AETHYME_REVIEW_WRITES", &self.writes)
            .env("AETHYME_REVIEW_HEAD", head)
            .env("AETHYME_REVIEW_DRAFT", draft.to_string())
            .env("AETHYME_REVIEW_DECISION", decision)
            .output()
            .unwrap()
    }

    fn writes(&self) -> String {
        std::fs::read_to_string(&self.writes).unwrap_or_default()
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
