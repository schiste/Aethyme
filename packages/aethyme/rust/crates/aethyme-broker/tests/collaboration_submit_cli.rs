//! #660: opt-in collaboration capture at the submit boundary, through the
//! CLI. Off is the legacy submit; advisory never changes the legacy verdict
//! or exit code; required refuses the submit before anything is queued.
//!
//! Every process gets its own host state, cache and home, so no case reads
//! or writes the developer's real collaboration state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    cache: tempfile::TempDir,
    home: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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

impl Fixture {
    /// A repository with `config` as `.aethyme/config.toml` (None: no file)
    /// and optionally a gate that always fails.
    fn new(config: Option<&str>, failing_gate: bool) -> Self {
        let fixture = Self {
            repo: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        let repo = fixture.repo.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        git(repo, &["add", "-A"]);
        if failing_gate {
            // Gates are read from the committed tree.
            std::fs::write(
                repo.join(".aethyme/gates.toml"),
                "[[gate]]\nname = \"always-fails\"\ncommand = \"exit 7\"\n",
            )
            .unwrap();
            git(repo, &["add", "-f", ".aethyme/gates.toml"]);
        }
        git(repo, &["commit", "-qm", "init"]);
        if let Some(config) = config {
            fixture.configure(config);
        }
        fixture
    }

    /// A repository with an `origin` whose default branch holds the
    /// committed `config`, so the config is read from the committed copy.
    fn with_origin(config: &str) -> (Self, tempfile::TempDir) {
        let fixture = Self::new(None, false);
        let repo = fixture.repo.path();
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        fixture.configure(config);
        git(repo, &["add", "-f", ".aethyme/config.toml"]);
        git(repo, &["commit", "-qm", "config"]);
        git(
            repo,
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo, &["push", "-q", "-u", "origin", "main"]);
        git(repo, &["remote", "set-head", "origin", "main"]);
        (fixture, origin)
    }

    fn configure(&self, config: &str) {
        std::fs::write(self.repo.path().join(".aethyme/config.toml"), config).unwrap();
    }

    fn cli(&self, args: &[&str], container: Option<&Path>) -> Output {
        let mut command = common::broker_cli(CLI, args);
        command
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env("AETHYME_HOST_CACHE_DIR", self.cache.path())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("AETHYME_WORKTREE_ROOT");
        if let Some(container) = container {
            command.env("AETHYME_WORKTREE_ROOT", container);
        }
        command.output().unwrap()
    }

    /// Start a session and commit one file in it.
    fn session(&self, name: &str) -> String {
        let output = self.cli(&["start", "--task", name, "--json"], None);
        assert!(
            output.status.success(),
            "start: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        std::fs::write(worktree.join(format!("{name}.txt")), "payload\n").unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", name]);
        started["id"].as_i64().unwrap().to_string()
    }

    fn submit_json(&self, session: &str, container: Option<&Path>) -> (i32, serde_json::Value) {
        let (code, value, _) = self.submit_raw(session, container);
        (code, value)
    }

    /// The exit code, the parsed JSON and its top-level keys in printed order
    /// (the parsed map is sorted, so order is read from the text).
    fn submit_raw(
        &self,
        session: &str,
        container: Option<&Path>,
    ) -> (i32, serde_json::Value, Vec<String>) {
        let output = self.cli(&["submit", "--session", session, "--json"], container);
        let order = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("  \""))
            .filter_map(|line| line.split_once("\":").map(|(key, _)| key.to_string()))
            .collect();
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{error}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value, order)
    }

    fn queued_entries(&self) -> i64 {
        rusqlite::Connection::open(self.repo.path().join(".aethyme/broker.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM merge_queue", [], |row| row.get(0))
            .unwrap()
    }

    fn collaboration(&self) -> PathBuf {
        self.state.path().join("collaboration")
    }
}

const ADVISORY: &str = "[collaboration]\ncapture = \"advisory\"\nproject = \"proj-test\"\n";
const REQUIRED: &str = "[collaboration]\ncapture = \"required\"\nproject = \"proj-test\"\n";

/// Disabled in each way it can be: the legacy JSON, exit code and verdict,
/// no capture line, and no collaboration state written.
#[test]
fn off_is_the_legacy_submit() {
    let advisory = Fixture::new(Some(ADVISORY), false);
    let session = advisory.session("work");
    let (_, _, mut legacy_keys) = advisory.submit_raw(&session, None);
    assert_eq!(legacy_keys.pop().as_deref(), Some("collaboration_capture"));

    for config in [
        None,
        Some("[promote]\nmode = \"auto\"\n"),
        Some("[collaboration]\nproject = \"proj-test\"\n"),
        Some("[collaboration]\ncapture = \"off\"\nproject = \"proj-test\"\n"),
    ] {
        let fixture = Fixture::new(config, false);
        let session = fixture.session("work");
        let (code, outcome, order) = fixture.submit_raw(&session, None);
        assert_eq!(code, 0, "{config:?}");
        assert_eq!(outcome["promoted"], true, "{config:?}");
        // The same fields, in the same order, as the outcome inside an
        // advisory report: the legacy object is not restructured.
        assert_eq!(order, legacy_keys, "{config:?}");
        assert!(!fixture.collaboration().exists(), "{config:?}");

        let session = fixture.session("text");
        let text = fixture.cli(&["submit", "--session", &session], None);
        assert!(text.status.success());
        assert!(
            !String::from_utf8_lossy(&text.stdout).contains("collaboration capture"),
            "{config:?}"
        );
    }
}

#[test]
fn advisory_capture_reports_a_receipt_beside_the_legacy_verdict() {
    let fixture = Fixture::new(Some(ADVISORY), false);
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, None);
    assert_eq!(code, 0);
    assert_eq!(outcome["promoted"], true);
    let capture = &outcome["collaboration_capture"];
    assert_eq!(capture["schema"], "aethyme.submit-capture/experimental-v0");
    assert_eq!(capture["policy"], "advisory");
    assert_eq!(capture["status"], "acknowledged", "{capture:#}");
    assert_eq!(capture["receipt"]["status"], "retained_local");
    assert!(
        capture["receipt"]["contribution"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(fixture.collaboration().join("proj-test/state.db").is_file());

    let session = fixture.session("text");
    let text = fixture.cli(&["submit", "--session", &session], None);
    assert!(text.status.success());
    assert!(
        String::from_utf8_lossy(&text.stdout)
            .contains("collaboration capture (advisory): retained_local"),
        "{}",
        String::from_utf8_lossy(&text.stdout)
    );
}

/// A failed advisory capture is reported with its next action and changes
/// nothing about the legacy outcome, in either direction of the verdict.
#[test]
fn a_failed_advisory_capture_never_changes_the_legacy_verdict() {
    // No project: not configured, still promoted with exit 0.
    let fixture = Fixture::new(Some("[collaboration]\ncapture = \"advisory\"\n"), false);
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, None);
    assert_eq!(code, 0);
    assert_eq!(outcome["promoted"], true);
    let capture = &outcome["collaboration_capture"];
    assert_eq!(capture["status"], "not_configured");
    assert_eq!(capture["code"], "no_project");
    assert!(capture["next_action"].as_str().unwrap().contains("project"));

    // A refused state root: the worktree container is the host state
    // directory, so the collaboration root would be one of its children.
    let fixture = Fixture::new(Some(ADVISORY), false);
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, Some(fixture.state.path()));
    assert_eq!(code, 0);
    assert_eq!(outcome["promoted"], true);
    let capture = &outcome["collaboration_capture"];
    assert_eq!(capture["code"], "overlaps_cleanup_root", "{capture:#}");
    assert!(capture["next_action"].is_string());
    assert!(!fixture.collaboration().exists());
    // The detail names the problem without host paths.
    let detail = capture["detail"].as_str().unwrap();
    for path in [
        fixture.state.path(),
        fixture.repo.path(),
        fixture.home.path(),
    ] {
        for spelling in [path.to_path_buf(), path.canonicalize().unwrap()] {
            assert!(!detail.contains(spelling.to_str().unwrap()), "{detail}");
        }
    }
    assert!(detail.contains("<host state>"), "{detail}");

    // A failing gate keeps its exit code; the capture still succeeds.
    let legacy = Fixture::new(None, true);
    let session = legacy.session("work");
    let (legacy_code, legacy_outcome) = legacy.submit_json(&session, None);
    let fixture = Fixture::new(Some(ADVISORY), true);
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, None);
    assert_ne!(legacy_code, 0);
    assert_eq!(code, legacy_code);
    assert_eq!(
        outcome["entry"]["status"],
        legacy_outcome["entry"]["status"]
    );
    assert_eq!(outcome["promoted"], false);
    assert_eq!(outcome["collaboration_capture"]["status"], "acknowledged");
}

/// Required capture runs first; when it is not acknowledged nothing is
/// queued, gated or promoted, and the refusal says why.
#[test]
fn a_failed_required_capture_refuses_the_submit_before_anything_runs() {
    for (config, code) in [
        (
            "[collaboration]\ncapture = \"required\"\n".to_string(),
            "no_project",
        ),
        (
            "[collaboration]\ncapture = \"required-v2\"\nproject = \"proj-test\"\n".to_string(),
            "unsupported_policy",
        ),
    ] {
        let fixture = Fixture::new(Some(&config), false);
        let session = fixture.session("work");
        let before = git(fixture.repo.path(), &["for-each-ref", "refs/heads/aethyme"]);
        let (exit, report) = fixture.submit_json(&session, None);
        assert_eq!(exit, 3, "{config}");
        assert_eq!(report["submitted"], false);
        assert_eq!(report["collaboration_capture"]["code"], code);
        assert!(report.get("entry").is_none(), "{report:#}");
        assert_eq!(fixture.queued_entries(), 0, "{config}");
        assert_eq!(
            git(fixture.repo.path(), &["for-each-ref", "refs/heads/aethyme"]),
            before
        );
    }
}

#[test]
fn an_acknowledged_required_capture_lets_the_submit_proceed() {
    let fixture = Fixture::new(Some(REQUIRED), false);
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, None);
    assert_eq!(code, 0);
    assert_eq!(outcome["promoted"], true);
    assert_eq!(outcome["collaboration_capture"]["policy"], "required");
    assert_eq!(outcome["collaboration_capture"]["status"], "acknowledged");
}

/// Resubmitting the same commits against the same integration tip is the
/// same capture operation and answers with the same receipt.
#[test]
fn a_retried_submit_gets_the_same_receipt() {
    let fixture = Fixture::new(
        Some(&format!("[promote]\nmode = \"verify-only\"\n\n{ADVISORY}")),
        false,
    );
    let session = fixture.session("work");
    let (_, first) = fixture.submit_json(&session, None);
    let (_, second) = fixture.submit_json(&session, None);
    let (first, second) = (
        &first["collaboration_capture"],
        &second["collaboration_capture"],
    );
    assert_eq!(first["status"], "acknowledged", "{first:#}");
    assert_eq!(first["operation_id"], second["operation_id"]);
    assert_eq!(first["receipt"], second["receipt"]);
}

/// Verify-only repositories verify against the fetched default branch and
/// refresh integration onto it. The capture uses the same base, so a resubmit
/// after the refresh is the same operation, and its base is the commit the
/// submit verified against.
#[test]
fn the_capture_base_is_the_base_the_submit_verified_against() {
    for policy in ["advisory", "required"] {
        let (fixture, origin) = Fixture::with_origin(&format!(
            "[promote]\nmode = \"verify-only\"\n\n[collaboration]\ncapture = \"{policy}\"\nproject = \"proj-test\"\n"
        ));
        let started = fixture.cli(&["start", "--task", "work", "--json"], None);
        assert!(started.status.success());
        let started: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        let session = started["id"].as_i64().unwrap().to_string();

        // Upstream moves on; integration still points at the old tip.
        let other = tempfile::tempdir().unwrap();
        git(
            other.path(),
            &["clone", "-q", origin.path().to_str().unwrap(), "."],
        );
        std::fs::write(other.path().join("upstream.txt"), "upstream\n").unwrap();
        git(other.path(), &["add", "-A"]);
        git(other.path(), &["commit", "-qm", "upstream"]);
        git(other.path(), &["push", "-q", "origin", "HEAD:main"]);
        git(fixture.repo.path(), &["fetch", "-q", "origin"]);
        let upstream = git(fixture.repo.path(), &["rev-parse", "origin/main"]);
        // The session is cut from upstream, not from the lagging integration.
        git(&worktree, &["merge", "-q", "--ff-only", "origin/main"]);
        std::fs::write(worktree.join("work.txt"), "payload\n").unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", "work"]);

        let (_, first) = fixture.submit_json(&session, None);
        let (_, second) = fixture.submit_json(&session, None);
        let capture = &first["collaboration_capture"];
        assert_eq!(capture["status"], "acknowledged", "{policy}: {capture:#}");
        assert_eq!(first["verified_against"]["commit"], upstream.as_str());
        assert_eq!(capture["base_commit"], upstream.as_str(), "{policy}");
        assert_eq!(
            capture["operation_id"], second["collaboration_capture"]["operation_id"],
            "{policy}"
        );
        assert_eq!(
            capture["receipt"],
            second["collaboration_capture"]["receipt"]
        );
    }
}

/// The committed default branch decides the policy: a working copy that
/// says off does not switch off a committed `required`.
#[test]
fn the_committed_policy_wins_over_the_working_copy() {
    let (fixture, _origin) = Fixture::with_origin(REQUIRED);
    fixture.configure("[collaboration]\ncapture = \"off\"\n");
    let session = fixture.session("work");
    let (code, outcome) = fixture.submit_json(&session, None);
    assert_eq!(code, 0);
    let capture = &outcome["collaboration_capture"];
    assert_eq!(capture["policy"], "required", "{outcome:#}");
    assert_eq!(capture["config_source"], "committed");
    assert_eq!(capture["status"], "acknowledged");
}

/// In text mode the legacy verdict is printed before the advisory capture
/// runs, so a slow or failing capture cannot hide or delay it.
#[test]
fn the_legacy_verdict_is_printed_before_the_advisory_capture() {
    let fixture = Fixture::new(Some(ADVISORY), false);
    let session = fixture.session("work");
    let output = fixture.cli(&["submit", "--session", &session], None);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let verdict = stdout.find("What now").expect("legacy verdict");
    let capture = stdout
        .find("collaboration capture (advisory)")
        .expect("capture line");
    assert!(verdict < capture, "{stdout}");
}
