//! Built-in broker smoke test for first-run confidence checks.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::gates::GateRunOutcome;
use crate::types::{GateFailureClass, GateStatus, MergeStatus};
use crate::{Broker, BrokerOpError};

const CHAU7_SKIP_MESSAGE: &str = "this test is designed to run in Chau7";
const FIXTURE_GATE_NAME: &str = "quick-test-fixture";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuickTestMode {
    Generic,
    Chau7,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Chau7Probe {
    pub detected: bool,
    /// Environment marker names only; values can contain session ids.
    pub markers: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QuickTestStep {
    pub name: &'static str,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuickTestOptions {
    pub with_gate: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QuickTestGateOutcome {
    pub gate: String,
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cached: bool,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QuickTestGateReport {
    pub gate_name: String,
    pub passing_entry_id: i64,
    pub passing_outcomes: Vec<QuickTestGateOutcome>,
    pub failing_entry_id: i64,
    pub failing_entry_status: MergeStatus,
    pub failing_outcomes: Vec<QuickTestGateOutcome>,
}

#[derive(Debug, serde::Serialize)]
pub struct QuickTestReport {
    pub ok: bool,
    pub skipped: bool,
    pub mode: QuickTestMode,
    pub with_gate: bool,
    pub message: String,
    pub temp_repo: String,
    pub temp_repo_removed: bool,
    pub chau7: Chau7Probe,
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub integration_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_fixture: Option<QuickTestGateReport>,
    pub steps: Vec<QuickTestStep>,
}

#[derive(Debug, thiserror::Error)]
pub enum QuickTestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Broker(#[from] BrokerOpError),
    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },
    #[error("aethyme init did not certify the temporary repo")]
    InitNotCertified,
    #[error(
        "quick-test broker database escaped its owned fixture: expected {expected}, resolved {resolved}"
    )]
    BrokerDatabaseRedirected {
        expected: PathBuf,
        resolved: PathBuf,
    },
    #[error("broker submit did not promote the smoke commit: {reason}")]
    SubmitNotPromoted { reason: String },
    #[error("broker gated quick test did not run the fixture gate")]
    GateNotRun,
    #[error("broker gated quick test fixture gate did not pass")]
    GateDidNotPass,
    #[error("broker gated quick test failing fixture was not rejected")]
    GateFailureNotRejected,
    #[error("broker gated quick test fixture gate did not fail")]
    GateDidNotFail,
}

pub fn run_broker_quick_test(mode: QuickTestMode) -> Result<QuickTestReport, QuickTestError> {
    run_broker_quick_test_with_options(mode, QuickTestOptions::default())
}

pub fn run_broker_quick_test_with_options(
    mode: QuickTestMode,
    options: QuickTestOptions,
) -> Result<QuickTestReport, QuickTestError> {
    let chau7 = detect_chau7_environment();
    if mode == QuickTestMode::Chau7 && !chau7.detected {
        return Ok(QuickTestReport {
            ok: false,
            skipped: true,
            mode,
            with_gate: options.with_gate,
            message: CHAU7_SKIP_MESSAGE.into(),
            temp_repo: String::new(),
            temp_repo_removed: false,
            chau7,
            session_id: None,
            queue_entry_id: None,
            integration_head: None,
            gate_fixture: None,
            steps: Vec::new(),
        });
    }

    let temp_repo = TempRepo::create()?;
    let mut steps = Vec::new();
    steps.push(step(
        "create-temp-repo",
        "pass",
        temp_repo.path().display().to_string(),
    ));

    git(temp_repo.path(), &["init", "-q", "-b", "main"])?;
    git(
        temp_repo.path(),
        &["config", "user.email", "smoke@example.invalid"],
    )?;
    git(temp_repo.path(), &["config", "user.name", "Aethyme Smoke"])?;
    std::fs::write(temp_repo.path().join("README.md"), "hello\n")?;
    git(temp_repo.path(), &["add", "README.md"])?;
    git(temp_repo.path(), &["commit", "-q", "-m", "initial commit"])?;
    steps.push(step("git-bootstrap", "pass", "initial commit on main"));

    temp_repo.ensure_broker_database_is_owned()?;
    let init = crate::init::guided_init(temp_repo.path())?;
    temp_repo.ensure_broker_database_is_owned()?;
    if !init.certified() {
        return Err(QuickTestError::InitNotCertified);
    }
    git(temp_repo.path(), &["add", "-A"])?;
    git(
        temp_repo.path(),
        &["commit", "-q", "-m", "chore: initialize aethyme broker"],
    )?;
    steps.push(step("aethyme-init", "pass", "scaffold committed"));

    if options.with_gate {
        install_gate_fixture(temp_repo.path())?;
        git(
            temp_repo.path(),
            &["add", ".aethyme/gates.toml", "PASS_GATE"],
        )?;
        git(
            temp_repo.path(),
            &["commit", "-q", "-m", "test: add broker gate fixture"],
        )?;
        steps.push(step(
            "gate-fixture",
            "pass",
            format!("installed {FIXTURE_GATE_NAME}"),
        ));
    }

    // The quick test wrote this repository's policy itself, so it approves
    // it itself; the trust record lives in the owned fixture storage.
    let policy = crate::broker::gate_trust::policy_at_root(temp_repo.path())?;
    if policy.runs_commands() {
        let record = crate::broker::gate_trust::record_path_for_host_state(
            temp_repo.path(),
            &temp_repo.storage().host_state_dir(),
        )?;
        crate::broker::gate_trust::record_trust_at_path(
            temp_repo.path(),
            &record,
            &[&policy.policy_sha256],
            "quick_test",
        )?;
    }

    let mut broker = temp_repo.open_broker()?;
    let session = broker.adopt(temp_repo.path(), Some("broker quick test"))?;
    steps.push(step(
        "broker-adopt",
        "pass",
        format!("session {}", session.id),
    ));
    let baseline_head = broker.integration_head()?.1;
    steps.push(step(
        "integration-baseline",
        "pass",
        format!("baseline {}", short_commit(&baseline_head)),
    ));

    std::fs::OpenOptions::new()
        .append(true)
        .open(temp_repo.path().join("README.md"))?
        .write_all(b"world\n")?;
    git(temp_repo.path(), &["add", "README.md"])?;
    git(
        temp_repo.path(),
        &["commit", "-q", "-m", "test: broker quick change"],
    )?;
    steps.push(step(
        "smoke-commit",
        "pass",
        "committed one broker-owned change",
    ));

    let outcome = broker.submit(session.id)?;
    if !outcome.promoted || !outcome.conflicts.is_empty() {
        return Err(QuickTestError::SubmitNotPromoted {
            reason: describe_unpromoted(&outcome.conflicts, &outcome.gate_outcomes),
        });
    }
    if options.with_gate {
        let gate = find_gate_outcome(&outcome.gate_outcomes).ok_or(QuickTestError::GateNotRun)?;
        if gate.status != GateStatus::Pass {
            return Err(QuickTestError::GateDidNotPass);
        }
    }
    let integration_head = broker.integration_head()?.1;
    steps.push(step(
        "broker-submit",
        "pass",
        if options.with_gate {
            format!(
                "entry {} promoted after {FIXTURE_GATE_NAME} pass",
                outcome.entry.id
            )
        } else {
            format!("entry {} promoted", outcome.entry.id)
        },
    ));

    let gate_fixture = if options.with_gate {
        std::fs::remove_file(temp_repo.path().join("PASS_GATE"))?;
        std::fs::OpenOptions::new()
            .append(true)
            .open(temp_repo.path().join("README.md"))?
            .write_all(b"gate failure variant\n")?;
        git(temp_repo.path(), &["add", "README.md"])?;
        git(temp_repo.path(), &["rm", "-q", "PASS_GATE"])?;
        git(
            temp_repo.path(),
            &["commit", "-q", "-m", "test: broker quick gate failure"],
        )?;
        steps.push(step(
            "failing-gate-commit",
            "pass",
            "committed a change that makes the fixture gate fail",
        ));

        let failing = broker.submit(session.id)?;
        if failing.promoted
            || !failing.conflicts.is_empty()
            || failing.entry.status != MergeStatus::Rejected
        {
            return Err(QuickTestError::GateFailureNotRejected);
        }
        let failing_gate =
            find_gate_outcome(&failing.gate_outcomes).ok_or(QuickTestError::GateNotRun)?;
        if failing_gate.status != GateStatus::Fail {
            return Err(QuickTestError::GateDidNotFail);
        }
        steps.push(step(
            "broker-submit-failing-gate",
            "pass",
            format!("entry {} rejected by {FIXTURE_GATE_NAME}", failing.entry.id),
        ));

        Some(QuickTestGateReport {
            gate_name: FIXTURE_GATE_NAME.into(),
            passing_entry_id: outcome.entry.id,
            passing_outcomes: gate_outcomes(&outcome.gate_outcomes),
            failing_entry_id: failing.entry.id,
            failing_entry_status: failing.entry.status,
            failing_outcomes: gate_outcomes(&failing.gate_outcomes),
        })
    } else {
        None
    };

    let temp_path = temp_repo.path().display().to_string();
    drop(broker);
    temp_repo.remove()?;

    Ok(QuickTestReport {
        ok: true,
        skipped: false,
        mode,
        with_gate: options.with_gate,
        message: "broker quick test passed".into(),
        temp_repo: temp_path,
        temp_repo_removed: true,
        chau7,
        session_id: Some(session.id),
        queue_entry_id: Some(outcome.entry.id),
        integration_head: Some(integration_head),
        gate_fixture,
        steps,
    })
}

pub fn detect_chau7_environment() -> Chau7Probe {
    let pairs: Vec<(String, String)> = std::env::vars().collect();
    detect_chau7_from_pairs(
        pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    )
}

fn detect_chau7_from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Chau7Probe {
    let mut markers = Vec::new();
    for (key, value) in pairs {
        let detected = key.starts_with("CHAU7_")
            || (key == "TERM_PROGRAM" && value == "Chau7")
            || (key == "__CFBundleIdentifier" && value.contains("chau7"))
            || (key == "ANTHROPIC_EXTRA_HEADERS" && value.contains("X-Chau7-"));
        if detected && !markers.iter().any(|seen| seen == key) {
            markers.push(key.to_string());
        }
    }
    Chau7Probe {
        detected: !markers.is_empty(),
        markers,
    }
}

fn step(name: &'static str, status: &'static str, detail: impl Into<String>) -> QuickTestStep {
    QuickTestStep {
        name,
        status,
        detail: detail.into(),
    }
}

fn install_gate_fixture(repo: &Path) -> Result<(), std::io::Error> {
    std::fs::write(repo.join("PASS_GATE"), "ok\n")?;
    std::fs::write(
        repo.join(".aethyme/gates.toml"),
        format!(
            r#"[[gate]]
name = "{FIXTURE_GATE_NAME}"
command = "test -f PASS_GATE"
cost = 1
cache = false
"#
        ),
    )?;
    Ok(())
}

/// Why a submission did not promote, in the terms a caller needs.
///
/// A gate that refuses *before spawning* -- disk headroom under
/// `DEFAULT_GATE_HEADROOM_BYTES`, a resource lease it could not take -- records
/// `error` with a failure class and never runs the command. Reported as a bare
/// "did not promote", that is indistinguishable from a gate that ran and failed
/// the change, so a full disk reads as a broken diff (#270). The evidence is
/// already on the outcome; this states it.
fn describe_unpromoted(conflicts: &[String], gates: &[GateRunOutcome]) -> String {
    let mut parts = Vec::new();
    if !conflicts.is_empty() {
        parts.push(format!("conflicts: {}", conflicts.join(", ")));
    }
    for gate in gates {
        if gate.status != GateStatus::Pass {
            let class = gate
                .failure_class
                .map(|class| format!(" [{}]", class.as_str()))
                .unwrap_or_default();
            let log = gate
                .log_path
                .as_deref()
                .map(|path| format!(" log={path}"))
                .unwrap_or_default();
            parts.push(format!(
                "gate {} {}{}{}",
                gate.gate,
                gate.status.as_str(),
                class,
                log
            ));
        }
    }
    if parts.is_empty() {
        "no conflict or gate evidence was recorded".to_string()
    } else {
        parts.join("; ")
    }
}

fn find_gate_outcome(outcomes: &[GateRunOutcome]) -> Option<&GateRunOutcome> {
    outcomes
        .iter()
        .find(|outcome| outcome.gate == FIXTURE_GATE_NAME)
}

fn gate_outcomes(outcomes: &[GateRunOutcome]) -> Vec<QuickTestGateOutcome> {
    outcomes
        .iter()
        .map(|outcome| QuickTestGateOutcome {
            gate: outcome.gate.clone(),
            tree_hash: outcome.tree_hash.clone(),
            status: outcome.status,
            failure_class: outcome.failure_class,
            cached: outcome.cached,
            exit_code: outcome.exit_code,
        })
        .collect()
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, QuickTestError> {
    let output = crate::git::git_command()
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(QuickTestError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

struct TempRepo {
    path: PathBuf,
    storage: Option<crate::test_storage::TestStorage>,
}

impl TempRepo {
    fn create() -> Result<Self, std::io::Error> {
        let storage = crate::test_storage::TestStorage::try_new()?;
        let path = storage.root().join("repository");
        if let Err(error) = std::fs::create_dir(&path) {
            crate::warn_unrecorded(
                "remove incomplete quick-test fixture storage",
                storage.close(),
            );
            return Err(error);
        }
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                crate::warn_unrecorded(
                    "remove incomplete quick-test fixture storage",
                    storage.close(),
                );
                return Err(error);
            }
        };
        Ok(Self {
            path,
            storage: Some(storage),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn storage(&self) -> &crate::test_storage::TestStorage {
        self.storage.as_ref().expect("quick-test storage owner")
    }

    fn ensure_broker_database_is_owned(&self) -> Result<(), QuickTestError> {
        let expected = self.path.join(crate::BROKER_DB_RELPATH);
        let resolved = crate::broker_db_path(&self.path)
            .map_err(|error| QuickTestError::Broker(BrokerOpError::from(error)))?;
        if resolved != expected {
            return Err(QuickTestError::BrokerDatabaseRedirected { expected, resolved });
        }
        Ok(())
    }

    fn open_broker(&self) -> Result<Broker, QuickTestError> {
        self.ensure_broker_database_is_owned()?;
        self.storage()
            .open(&self.path)
            .map_err(QuickTestError::Broker)
    }

    fn remove(mut self) -> Result<(), std::io::Error> {
        self.storage
            .take()
            .expect("quick-test storage owner")
            .close()
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        if let Some(storage) = self.storage.take() {
            crate::warn_unrecorded("remove quick-test fixture storage", storage.close());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chau7_without_exposing_marker_values() {
        let probe = detect_chau7_from_pairs([
            ("CHAU7_SESSION_ID", "secret-session"),
            ("TERM_PROGRAM", "Chau7"),
            ("OTHER", "X-Chau7-Session: secret"),
        ]);

        assert!(probe.detected);
        assert_eq!(probe.markers, vec!["CHAU7_SESSION_ID", "TERM_PROGRAM"]);
        assert!(!probe.markers.iter().any(|marker| marker.contains("secret")));
    }

    #[test]
    fn generic_quick_test_promotes_in_disposable_repo() {
        let report = run_broker_quick_test(QuickTestMode::Generic).unwrap();

        assert!(report.ok);
        assert!(!report.skipped);
        assert!(!report.with_gate);
        assert!(report.temp_repo_removed);
        assert!(report.session_id.is_some());
        assert!(report.queue_entry_id.is_some());
        assert!(report.integration_head.is_some());
        assert!(report.gate_fixture.is_none());
        assert!(report.steps.iter().any(|step| step.name == "broker-submit"));
        assert_fixture_container_removed(&report);
    }

    #[test]
    fn gated_quick_test_promotes_pass_and_rejects_fail() {
        let report = run_broker_quick_test_with_options(
            QuickTestMode::Generic,
            QuickTestOptions { with_gate: true },
        )
        .unwrap();

        assert!(report.ok);
        assert!(report.with_gate);
        assert!(report.temp_repo_removed);
        assert_fixture_container_removed(&report);
        let gate = report.gate_fixture.expect("gate fixture report");
        assert_eq!(gate.gate_name, FIXTURE_GATE_NAME);
        assert!(
            gate.passing_outcomes
                .iter()
                .any(|outcome| outcome.gate == FIXTURE_GATE_NAME
                    && outcome.status == GateStatus::Pass
                    && !outcome.cached)
        );
        assert_eq!(gate.failing_entry_status, MergeStatus::Rejected);
        assert!(
            gate.failing_outcomes
                .iter()
                .any(|outcome| outcome.gate == FIXTURE_GATE_NAME
                    && outcome.status == GateStatus::Fail
                    && !outcome.cached)
        );
        assert!(
            report
                .steps
                .iter()
                .any(|step| step.name == "broker-submit-failing-gate")
        );
    }

    #[test]
    fn early_quick_test_failure_removes_the_owned_fixture_container() {
        let temp_repo = TempRepo::create().unwrap();
        let owner = temp_repo.storage().root().to_path_buf();
        let failure = git(temp_repo.path(), &["not-a-real-git-subcommand"]);
        assert!(matches!(failure, Err(QuickTestError::Git { .. })));

        drop(temp_repo);

        assert!(!owner.exists());
    }

    #[test]
    fn hostile_broker_database_child_environment_is_refused_before_database_creation() {
        const CHILD_MARKER: &str = "AETHYME_TEST_QUICK_TEST_BROKER_DB_CHILD";
        if std::env::var(CHILD_MARKER).is_ok_and(|value| value == "child") {
            let external_database = PathBuf::from(std::env::var_os(crate::BROKER_DB_ENV).unwrap());
            let result = run_broker_quick_test(QuickTestMode::Generic);
            assert!(
                matches!(
                    &result,
                    Err(QuickTestError::BrokerDatabaseRedirected { resolved, .. })
                        if resolved == &external_database
                ),
                "unexpected quick-test result: {result:?}"
            );
            assert!(!external_database.exists());
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let external_database = parent.path().join("external-broker.db");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "quick_test::tests::hostile_broker_database_child_environment_is_refused_before_database_creation",
            )
            .arg("--nocapture")
            .env(CHILD_MARKER, "child")
            .env(crate::BROKER_DB_ENV, &external_database)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!external_database.exists());
    }

    #[test]
    fn failure_after_trust_and_broker_creation_removes_the_owned_container() {
        let temp_repo = TempRepo::create().unwrap();
        let owner = temp_repo.storage().root().to_path_buf();
        git(temp_repo.path(), &["init", "-q", "-b", "main"]).unwrap();
        git(
            temp_repo.path(),
            &["config", "user.email", "smoke@example.invalid"],
        )
        .unwrap();
        git(temp_repo.path(), &["config", "user.name", "Aethyme Smoke"]).unwrap();
        std::fs::write(temp_repo.path().join("README.md"), "hello\n").unwrap();
        git(temp_repo.path(), &["add", "README.md"]).unwrap();
        git(temp_repo.path(), &["commit", "-q", "-m", "initial commit"]).unwrap();

        temp_repo.ensure_broker_database_is_owned().unwrap();
        let init = crate::init::guided_init(temp_repo.path()).unwrap();
        assert!(init.certified());
        install_gate_fixture(temp_repo.path()).unwrap();
        git(
            temp_repo.path(),
            &["add", ".aethyme/gates.toml", "PASS_GATE"],
        )
        .unwrap();
        git(
            temp_repo.path(),
            &["commit", "-q", "-m", "test: add broker gate fixture"],
        )
        .unwrap();

        let policy = crate::broker::gate_trust::policy_at_root(temp_repo.path()).unwrap();
        let record = crate::broker::gate_trust::record_path_for_host_state(
            temp_repo.path(),
            &temp_repo.storage().host_state_dir(),
        )
        .unwrap();
        crate::broker::gate_trust::record_trust_at_path(
            temp_repo.path(),
            &record,
            &[&policy.policy_sha256],
            "quick_test",
        )
        .unwrap();
        assert!(record.is_file());

        let failure = (|| -> Result<(), QuickTestError> {
            let _broker = temp_repo.open_broker()?;
            Err(QuickTestError::GateDidNotPass)
        })();
        assert!(matches!(failure, Err(QuickTestError::GateDidNotPass)));

        drop(temp_repo);

        assert!(!owner.exists());
    }

    fn assert_fixture_container_removed(report: &QuickTestReport) {
        let repository = Path::new(&report.temp_repo);
        let owner = repository.parent().expect("repository is inside its owner");
        assert!(!repository.exists());
        assert!(!owner.exists());
    }
}

#[cfg(test)]
mod not_promoted_tests {
    use super::*;

    fn gate(name: &str, status: GateStatus, class: Option<GateFailureClass>) -> GateRunOutcome {
        GateRunOutcome {
            gate: name.to_string(),
            run_id: None,
            tree_hash: "t".into(),
            definition_hash: "d".into(),
            cache_provenance: None,
            resource_lease: None,
            managed_cache: None,
            broker_database: None,
            status,
            failure_class: class,
            cached: false,
            exit_code: None,
            duration_ms: None,
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: None,
            log_path: Some("/logs/gate.log".into()),
            environment: crate::GateEnvironment::default(),
            host_fault: false,
        }
    }

    /// The case that cost the diagnosis: a gate refused before spawning because
    /// the disk was under the headroom the gates enforce. Reported as a bare
    /// "did not promote", that is indistinguishable from a gate that ran and
    /// failed the change (#270).
    #[test]
    fn a_gate_that_refused_to_start_is_named_with_its_class() {
        let reason = describe_unpromoted(
            &[],
            &[gate(
                "cargo-test",
                GateStatus::Error,
                Some(GateFailureClass::ResourceContention),
            )],
        );
        assert!(reason.contains("cargo-test"), "{reason}");
        assert!(reason.contains("error"), "{reason}");
        assert!(
            reason.contains("resource_contention"),
            "the class is what distinguishes a starved machine from a bad diff: {reason}"
        );
        assert!(reason.contains("/logs/gate.log"), "{reason}");
    }

    #[test]
    fn conflicts_are_reported_and_passing_gates_are_not_noise() {
        let reason = describe_unpromoted(
            &["src/a.rs".to_string()],
            &[
                gate("fast", GateStatus::Pass, None),
                gate(
                    "slow",
                    GateStatus::Fail,
                    Some(GateFailureClass::TestFailure),
                ),
            ],
        );
        assert!(reason.contains("conflicts: src/a.rs"), "{reason}");
        assert!(reason.contains("slow"), "{reason}");
        assert!(
            !reason.contains("fast"),
            "a gate that passed is not a reason for refusal: {reason}"
        );
    }

    /// Never silently empty: an unexplained refusal must still say that the
    /// evidence was missing, rather than printing nothing after the colon.
    #[test]
    fn an_outcome_with_no_evidence_still_says_so() {
        let reason = describe_unpromoted(&[], &[]);
        assert!(reason.contains("no conflict or gate evidence"), "{reason}");
    }
}
