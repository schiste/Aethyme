//! #680 slice 1: `aethyme collab` through the built router (T33-T35, T57).
//!
//! Disabled unless `[collaboration]` enables it; a malformed or unknown
//! critical setting is refused; every subcommand's success and refusal
//! paths, with versioned JSON. Captures are made the way users make them:
//! an advisory `broker submit`. Every process gets its own host state,
//! cache, home and worktree container, so no case touches real state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_testkit::aethyme_bin;

const PROJECT: &str = "proj-collabtest";

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    cache: tempfile::TempDir,
    home: tempfile::TempDir,
    worktrees: tempfile::TempDir,
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

fn enabled_config() -> String {
    format!("[collaboration]\ncapture = \"advisory\"\nproject = \"{PROJECT}\"\n")
}

impl Fixture {
    fn new(config: Option<&str>) -> Self {
        let fixture = Self {
            repo: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            worktrees: tempfile::tempdir().unwrap(),
        };
        let repo = fixture.repo.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        if let Some(config) = config {
            fixture.configure(config);
        }
        fixture
    }

    fn configure(&self, config: &str) {
        std::fs::write(self.repo.path().join(".aethyme/config.toml"), config).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(aethyme_bin())
            .args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env("AETHYME_HOST_CACHE_DIR", self.cache.path())
            .env("AETHYME_WORKTREE_ROOT", self.worktrees.path())
            .env(
                "AETHYME_CHAU7_MCP_BRIDGE",
                "/__aethyme_test_no_chau7_bridge__",
            )
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .output()
            .unwrap()
    }

    /// Run with `--json`; the exit code and the single JSON object printed.
    fn json(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.run(&args);
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: stdout is not one JSON object ({error}):\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value)
    }

    fn collaboration_dir(&self) -> PathBuf {
        self.state.path().join("collaboration")
    }

    /// Plant an archive object nothing references, aged past the grace
    /// period, as an interrupted capture can leave.
    fn orphan_object(&self, bytes: &[u8]) -> PathBuf {
        use sha2::Digest;
        let hex = format!("{:x}", sha2::Sha256::digest(bytes));
        let path = self
            .collaboration_dir()
            .join(PROJECT)
            .join("objects/sha256")
            .join(&hex[..2])
            .join(&hex[2..]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        path
    }

    /// Capture a committed change the way users do: an advisory submit.
    /// Returns the operation ID and the contribution ID.
    fn capture(&self, name: &str) -> (String, String) {
        let (code, started) = self.json(&["broker", "start", "--task", name, "--short-name", name]);
        assert_eq!(code, 0, "{started}");
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/lib.rs"), format!("// {name}\n")).unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", name]);
        let session = started["id"].as_i64().unwrap().to_string();
        let (_, submitted) = self.json(&["broker", "submit", "--session", &session]);
        let capture = &submitted["collaboration_capture"];
        assert_eq!(capture["status"], "acknowledged", "{submitted:#}");
        (
            capture["operation_id"].as_str().unwrap().to_string(),
            capture["receipt"]["contribution"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }
}

const WRITE_COMMANDS: &[&[&str]] = &[
    &["collab", "capture", "recover"],
    &["collab", "capture", "abort", "--operation", "op-1"],
    &["collab", "capture", "receipt", "--operation", "op-1"],
    &["collab", "gc", "plan"],
    &["collab", "gc", "apply", "--confirm", "0000"],
    &["collab", "gc", "resume"],
    &["collab", "context", "--path", "src/lib.rs"],
    &[
        "collab",
        "brief",
        "attach",
        "--contribution",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "brief.json",
    ],
];

/// T33/T57: with no policy, status reads "disabled" and every other
/// subcommand refuses with the setting to change, creating nothing.
#[test]
fn without_a_policy_status_is_disabled_and_everything_else_refuses() {
    for config in [
        None,
        Some("[promote]\nmode = \"auto\"\n"),
        Some("[collaboration]\ncapture = \"off\"\n"),
    ] {
        let fixture = Fixture::new(config);
        let (code, status) = fixture.json(&["collab", "status"]);
        assert_eq!(code, 0);
        assert_eq!(status["schema"], "aethyme.collab-status/experimental-v0");
        assert_eq!(status["enabled"], false);
        assert_eq!(status["policy"], "off");
        for command in WRITE_COMMANDS {
            let (code, refusal) = fixture.json(command);
            assert_eq!(code, 3, "{command:?}: {refusal}");
            assert_eq!(refusal["schema"], "aethyme.collab-error/experimental-v0");
            assert_eq!(refusal["code"], "collaboration_disabled", "{command:?}");
            assert!(
                refusal["next_action"]
                    .as_str()
                    .unwrap()
                    .contains("aethyme collab enroll"),
                "{refusal}"
            );
        }
        assert!(!fixture.collaboration_dir().exists(), "{config:?}");
    }
}

/// T34/T57: a malformed or unknown critical policy is refused, never read
/// as disabled or as a weaker policy.
#[test]
fn a_malformed_or_unknown_policy_is_refused() {
    for (config, code) in [
        (
            "[collaboration\ncapture = \"advisory\"\n",
            "config_unreadable",
        ),
        ("collaboration = \"advisory\"\n", "unsupported_policy"),
        (
            "[collaboration]\ncapture = \"always\"\n",
            "unsupported_policy",
        ),
        (
            &*format!("{}brief_policy = \"required\"\n", enabled_config()),
            "unknown_setting",
        ),
    ] {
        let fixture = Fixture::new(Some(config));
        let (exit, status) = fixture.json(&["collab", "status"]);
        assert_eq!(exit, 0);
        assert_eq!(status["enabled"], false, "{config}");
        assert_eq!(status["policy"], "unsupported", "{config}");
        assert_eq!(status["policy_error"]["code"], code, "{config}");
        let (exit, refusal) = fixture.json(&["collab", "gc", "plan"]);
        assert_eq!(exit, 3);
        assert_eq!(refusal["code"], code, "{config}");
        assert!(!fixture.collaboration_dir().exists());
    }
}

/// An unknown setting matters only once capture is on: a repository that
/// has not enabled capture keeps the legacy submit.
#[test]
fn an_unknown_setting_without_capture_stays_disabled() {
    let fixture = Fixture::new(Some("[collaboration]\nflavour = \"x\"\n"));
    let (_, status) = fixture.json(&["collab", "status"]);
    assert_eq!(status["policy"], "off");
}

/// Enrollment is explicit: it mints a project key, and writes the section
/// only on --write and only into a config that does not mention it yet.
#[test]
fn enroll_mints_a_key_and_writes_only_when_asked() {
    let fixture = Fixture::new(None);
    let (code, printed) = fixture.json(&["collab", "enroll"]);
    assert_eq!(code, 0);
    assert_eq!(printed["schema"], "aethyme.collab-enroll/experimental-v0");
    let key = printed["project_key"].as_str().unwrap();
    assert!(key.starts_with("proj-") && key.len() == 31, "{key}");
    assert_eq!(
        printed["project_id"].as_str().unwrap(),
        key.replacen("proj-", "proj:", 1)
    );
    assert_eq!(printed["written"], false);
    assert!(!fixture.repo.path().join(".aethyme/config.toml").exists());

    let (code, written) = fixture.json(&["collab", "enroll", "--write"]);
    assert_eq!(code, 0);
    assert_eq!(written["written"], true);
    let config = std::fs::read_to_string(fixture.repo.path().join(".aethyme/config.toml")).unwrap();
    assert!(
        config.contains(written["project_key"].as_str().unwrap()),
        "{config}"
    );
    let (_, status) = fixture.json(&["collab", "status"]);
    assert_eq!(status["enabled"], true);
    assert_eq!(status["state"]["initialized"], false);
    assert!(!fixture.collaboration_dir().exists(), "status never writes");

    let (code, again) = fixture.json(&["collab", "enroll"]);
    assert_eq!(code, 3);
    assert_eq!(again["code"], "already_enrolled");

    let other = Fixture::new(Some("[collaboration]\ncapture = \"off\"\n"));
    let (code, refused) = other.json(&["collab", "enroll", "--write"]);
    assert_eq!(code, 3);
    assert_eq!(refused["code"], "already_configured");
}

/// A refused state root is reported by status and refuses every command,
/// with the safe next action.
#[test]
fn a_refused_state_root_is_reported_with_the_next_action() {
    let fixture = Fixture::new(Some(&enabled_config()));
    let output = Command::new(aethyme_bin())
        .args(["collab", "status", "--json"])
        .current_dir(fixture.repo.path())
        .env("HOME", fixture.home.path())
        .env("AETHYME_HOST_STATE_DIR", fixture.state.path())
        .env("AETHYME_HOST_CACHE_DIR", fixture.cache.path())
        // The worktree container is the host state directory itself.
        .env("AETHYME_WORKTREE_ROOT", fixture.state.path())
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["state"]["refusal"]["code"], "overlaps_cleanup_root");
    assert!(status["state"]["refusal"]["next_action"].is_string());
    assert!(!fixture.collaboration_dir().exists());
}

/// T35: the subcommands over a real advisory capture, success and refusal.
#[test]
fn the_subcommands_work_over_a_real_capture() {
    let fixture = Fixture::new(Some(&enabled_config()));
    let (operation, contribution) = fixture.capture("first");

    let (code, status) = fixture.json(&["collab", "status"]);
    assert_eq!(code, 0);
    assert_eq!(status["enabled"], true);
    assert_eq!(status["state"]["initialized"], true);
    assert_eq!(
        status["captures"]["by_state"]["acknowledged"], 1,
        "{status:#}"
    );
    assert!(status["state"]["receipt_label"].is_string());

    let (code, receipt) =
        fixture.json(&["collab", "capture", "receipt", "--operation", &operation]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(
        receipt["schema"],
        "aethyme.collab-capture-receipt/experimental-v0"
    );
    assert_eq!(receipt["status"], "retained_local");
    assert_eq!(receipt["contribution"], contribution.as_str());

    let (code, missing) = fixture.json(&["collab", "capture", "receipt", "--operation", "nope"]);
    assert_eq!(code, 3);
    assert_eq!(missing["code"], "no_receipt");

    let (code, refused) = fixture.json(&["collab", "capture", "abort", "--operation", &operation]);
    assert_eq!(code, 3, "{refused}");
    assert_eq!(refused["code"], "already_committed");
    assert!(
        refused["next_action"]
            .as_str()
            .unwrap()
            .contains("collab gc")
    );

    let (code, recovered) = fixture.json(&["collab", "capture", "recover"]);
    assert_eq!(code, 0);
    assert_eq!(
        recovered["schema"],
        "aethyme.collab-capture-recover/experimental-v0"
    );
    assert_eq!(recovered["recovered"], serde_json::json!([]));

    // A brief attaches to the contribution and comes back as untrusted data.
    let brief = fixture.repo.path().join("brief.json");
    std::fs::write(
        &brief,
        r#"{"intent": "Keep lib.rs tiny.", "decisions": [{"scope_ref": "src/lib.rs", "choice": "one comment", "reason": "fixture"}]}"#,
    )
    .unwrap();
    let brief_path = brief.to_str().unwrap();
    let (code, attached) = fixture.json(&[
        "collab",
        "brief",
        "attach",
        "--contribution",
        &contribution,
        brief_path,
    ]);
    assert_eq!(code, 0, "{attached}");
    assert_eq!(
        attached["schema"],
        "aethyme.collab-brief-attach/experimental-v0"
    );
    std::fs::write(&brief, r#"{"intent": ""}"#).unwrap();
    let (code, invalid) = fixture.json(&[
        "collab",
        "brief",
        "attach",
        "--contribution",
        &contribution,
        brief_path,
    ]);
    assert_eq!(code, 3);
    assert_eq!(invalid["code"], "invalid_brief");

    let (code, context) = fixture.json(&["collab", "context", "--path", "src/lib.rs"]);
    assert_eq!(code, 0, "{context}");
    assert_eq!(context["schema"], "aethyme.collab-context/experimental-v0");
    assert_eq!(context["absence_is_evidence"], false);
    let items = context["context"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{context:#}");
    assert_eq!(items[0]["brief"]["role"], "untrusted_data");
    let (_, again) = fixture.json(&["collab", "context", "--path", "src/lib.rs"]);
    assert_eq!(again["served"], "cache");
    let text = fixture.run(&["collab", "context", "--path", "src/lib.rs"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("untrusted data"), "{text}");
    let (code, out_of_range) = fixture.json(&[
        "collab",
        "context",
        "--path",
        "src/lib.rs",
        "--max-items",
        "0",
    ]);
    assert_eq!(code, 3);
    assert_eq!(out_of_range["code"], "budget_out_of_range");

    // Nothing is reclaimable while the capture's root is live, so the plan
    // is not recorded and cannot be applied.
    let (code, plan) = fixture.json(&["collab", "gc", "plan"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(plan["schema"], "aethyme.collab-gc-plan/experimental-v0");
    assert_eq!(plan["recorded"], false, "{plan:#}");
    let (code, unknown) = fixture.json(&[
        "collab",
        "gc",
        "apply",
        "--confirm",
        plan["digest"].as_str().unwrap(),
    ]);
    assert_eq!(code, 3);
    assert_eq!(unknown["code"], "unknown_plan");

    // An orphan object past its grace period is reclaimed.
    let orphan = fixture.orphan_object(b"orphaned bytes\n");
    let (code, plan) = fixture.json(&["collab", "gc", "plan"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(plan["recorded"], true, "{plan:#}");
    let digest = plan["digest"].as_str().unwrap().to_string();
    let (code, applied) = fixture.json(&["collab", "gc", "apply", "--confirm", &digest]);
    assert_eq!(code, 0, "{applied}");
    assert_eq!(applied["schema"], "aethyme.collab-gc-apply/experimental-v0");
    assert_eq!(
        applied["reclaimed"].as_array().unwrap().len(),
        1,
        "{applied:#}"
    );
    assert!(!orphan.exists());
    let (code, resumed) = fixture.json(&["collab", "gc", "resume"]);
    assert_eq!(code, 0);
    assert_eq!(
        resumed["schema"],
        "aethyme.collab-gc-resume/experimental-v0"
    );

    // The retained source is still promised after reclamation.
    let (_, receipt) = fixture.json(&["collab", "capture", "receipt", "--operation", &operation]);
    assert_eq!(receipt["status"], "retained_local");
}

#[test]
fn usage_errors_are_exit_2_and_help_has_no_side_effects() {
    let fixture = Fixture::new(Some(&enabled_config()));
    for args in [
        &["collab", "frobnicate"][..],
        &["collab", "gc", "apply"],
        &["collab", "capture", "abort"],
        &["collab", "context"],
        &["collab", "gc", "plan", "--bogus"],
    ] {
        let (code, error) = fixture.json(args);
        assert_eq!(code, 2, "{args:?}: {error}");
        assert_eq!(error["code"], "usage");
    }
    for args in [
        &["collab", "--help"][..],
        &["collab", "gc", "apply", "--help"],
    ] {
        let output = fixture.run(args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("aethyme collab status"));
    }
    assert!(!fixture.collaboration_dir().exists());
}

/// Under required capture, status shows the broker.db fence #660 raises,
/// read without raising it: before any broker command it is absent and the
/// next action says how it is raised; afterwards it is reported.
#[test]
fn status_reports_the_required_capture_fence_without_raising_it() {
    let fixture = Fixture::new(Some(&format!(
        "[collaboration]\ncapture = \"required\"\nproject = \"{PROJECT}\"\n"
    )));
    let (code, before) = fixture.json(&["collab", "status"]);
    assert_eq!(code, 0);
    assert_eq!(before["collaboration_fence"], serde_json::Value::Null);
    assert!(
        before["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action
                .as_str()
                .unwrap()
                .contains("raise the required-capture fence")),
        "{before:#}"
    );
    let (_, again) = fixture.json(&["collab", "status"]);
    assert_eq!(
        again["collaboration_fence"],
        serde_json::Value::Null,
        "status raised it"
    );

    let (code, _) = fixture.json(&["broker", "status"]);
    assert_eq!(code, 0);
    let (_, after) = fixture.json(&["collab", "status"]);
    let fence = &after["collaboration_fence"];
    assert!(
        fence["min_compatible_schema"].as_i64().unwrap() >= 50,
        "{after:#}"
    );
    assert!(fence["reason"].is_string());

    let advisory = Fixture::new(Some(&enabled_config()));
    let (_, status) = advisory.json(&["collab", "status"]);
    assert_eq!(status["collaboration_fence"], serde_json::Value::Null);
}
