//! Built-in end-to-end broker verification loop.

use std::path::Path;
use std::process::Command;

use crate::{Broker, BrokerOpError, DoctorReport, GitRepo, QuickTestReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyLoopStepStatus {
    Pass,
    Fail,
    Skip,
}

impl VerifyLoopStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }

    fn is_ok(self) -> bool {
        matches!(self, Self::Pass | Self::Skip)
    }
}

impl serde::Serialize for VerifyLoopStepStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, serde::Serialize)]
pub struct VerifyLoopStep {
    pub name: &'static str,
    pub status: VerifyLoopStepStatus,
    pub detail: String,
    pub duration_ms: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct VerifyLoopCommandReport {
    pub attempted: bool,
    pub command: Vec<String>,
    pub status: VerifyLoopStepStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub summary: String,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct VerifyLoopReport {
    pub ok: bool,
    pub integration_branch: String,
    pub tested_integration_head: String,
    pub current_integration_head: String,
    pub integration_moved: bool,
    pub message: String,
    pub steps: Vec<VerifyLoopStep>,
    pub quick_test: Option<QuickTestReport>,
    pub quick_test_error: Option<String>,
    pub doctor: Option<DoctorReport>,
    pub doctor_error: Option<String>,
    pub source_tests: VerifyLoopCommandReport,
}

impl Broker {
    /// One-command broker verification for operators: snapshot the
    /// integration branch, run the disposable smoke, run doctor, run the
    /// broker test suite when this is an Aethyme source checkout, then
    /// report whether integration moved during the run.
    pub fn verify_loop(&mut self) -> Result<VerifyLoopReport, BrokerOpError> {
        let source_root = self.main_root().to_path_buf();
        self.verify_loop_from(&source_root)
    }

    /// Same as [`Self::verify_loop`], but lets the CLI pass the checkout
    /// it was invoked from. In a linked worktree the broker state lives in
    /// the main checkout, while the source being verified is the current
    /// worktree.
    pub fn verify_loop_from(
        &mut self,
        source_root: &Path,
    ) -> Result<VerifyLoopReport, BrokerOpError> {
        let (integration_branch, tested_integration_head) = self.integration_head()?;
        let mut steps = Vec::new();
        steps.push(VerifyLoopStep {
            name: "integration-start",
            status: VerifyLoopStepStatus::Pass,
            detail: format!(
                "{} @ {}",
                integration_branch,
                short_commit(&tested_integration_head)
            ),
            duration_ms: 0,
        });

        let quick_start = now_ms();
        let (quick_test, quick_test_error, quick_status, quick_detail) =
            match crate::run_broker_quick_test(crate::QuickTestMode::Generic) {
                Ok(report) => {
                    let status = if report.ok && !report.skipped {
                        VerifyLoopStepStatus::Pass
                    } else if report.skipped {
                        VerifyLoopStepStatus::Skip
                    } else {
                        VerifyLoopStepStatus::Fail
                    };
                    let detail = report.message.clone();
                    (Some(report), None, status, detail)
                }
                Err(err) => (
                    None,
                    Some(err.to_string()),
                    VerifyLoopStepStatus::Fail,
                    err.to_string(),
                ),
            };
        steps.push(VerifyLoopStep {
            name: "quick-test",
            status: quick_status,
            detail: quick_detail,
            duration_ms: now_ms().saturating_sub(quick_start),
        });

        let doctor_start = now_ms();
        let (doctor, doctor_error, doctor_status, doctor_detail) = match self.doctor() {
            Ok(report) => {
                let status = if report.healthy() {
                    VerifyLoopStepStatus::Pass
                } else {
                    VerifyLoopStepStatus::Fail
                };
                let detail = if report.healthy() {
                    "doctor healthy".to_string()
                } else {
                    format!("doctor found problems: {}", report.version.status.as_str())
                };
                (Some(report), None, status, detail)
            }
            Err(err) => (
                None,
                Some(err.to_string()),
                VerifyLoopStepStatus::Fail,
                err.to_string(),
            ),
        };
        steps.push(VerifyLoopStep {
            name: "doctor",
            status: doctor_status,
            detail: doctor_detail,
            duration_ms: now_ms().saturating_sub(doctor_start),
        });

        let source_tests = run_source_tests(source_root);
        steps.push(VerifyLoopStep {
            name: "source-tests",
            status: source_tests.status,
            detail: source_tests.summary.clone(),
            duration_ms: source_tests.duration_ms,
        });

        let (_, current_integration_head) = self.integration_head()?;
        let integration_moved = tested_integration_head != current_integration_head;
        let stability_status = if integration_moved {
            VerifyLoopStepStatus::Fail
        } else {
            VerifyLoopStepStatus::Pass
        };
        let stability_detail = if integration_moved {
            format!(
                "tested old tip {}; current tip {}; rerun needed",
                short_commit(&tested_integration_head),
                short_commit(&current_integration_head)
            )
        } else {
            format!(
                "integration stayed at {}",
                short_commit(&current_integration_head)
            )
        };
        steps.push(VerifyLoopStep {
            name: "integration-stability",
            status: stability_status,
            detail: stability_detail,
            duration_ms: 0,
        });

        let ok = !integration_moved
            && quick_status == VerifyLoopStepStatus::Pass
            && doctor_status == VerifyLoopStepStatus::Pass
            && source_tests.status.is_ok();
        let message = if ok {
            format!(
                "broker verify-loop passed for {} @ {}",
                integration_branch,
                short_commit(&tested_integration_head)
            )
        } else if integration_moved {
            format!(
                "broker verify-loop tested old tip {}; current tip {}; rerun needed",
                short_commit(&tested_integration_head),
                short_commit(&current_integration_head)
            )
        } else {
            "broker verify-loop found problems".to_string()
        };

        Ok(VerifyLoopReport {
            ok,
            integration_branch,
            tested_integration_head,
            current_integration_head,
            integration_moved,
            message,
            steps,
            quick_test,
            quick_test_error,
            doctor,
            doctor_error,
            source_tests,
        })
    }
}

fn run_source_tests(root: &Path) -> VerifyLoopCommandReport {
    let source_root = GitRepo::discover(root)
        .map(|repo| repo.root().to_path_buf())
        .unwrap_or_else(|_| root.to_path_buf());
    let manifest = source_root.join("packages/aethyme/rust/Cargo.toml");
    let command = vec![
        "cargo".to_string(),
        "test".to_string(),
        "--manifest-path".to_string(),
        manifest.to_string_lossy().into_owned(),
        "-p".to_string(),
        "aethyme-broker".to_string(),
    ];
    if !is_aethyme_source_checkout(&source_root) {
        return VerifyLoopCommandReport {
            attempted: false,
            command,
            status: VerifyLoopStepStatus::Skip,
            exit_code: None,
            duration_ms: 0,
            summary: "not an Aethyme source checkout; broker source tests skipped".into(),
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        };
    }

    let start = now_ms();
    let output = Command::new("cargo")
        .args(&command[1..])
        .current_dir(&source_root)
        .output();
    let duration_ms = now_ms().saturating_sub(start);
    match output {
        Ok(output) => {
            let stdout_tail = tail_lines(&String::from_utf8_lossy(&output.stdout), 12);
            let stderr_tail = tail_lines(&String::from_utf8_lossy(&output.stderr), 12);
            let status = if output.status.success() {
                VerifyLoopStepStatus::Pass
            } else {
                VerifyLoopStepStatus::Fail
            };
            let summary = first_non_empty(&stdout_tail)
                .or_else(|| first_non_empty(&stderr_tail))
                .unwrap_or_else(|| {
                    if output.status.success() {
                        "cargo test passed".to_string()
                    } else {
                        "cargo test failed".to_string()
                    }
                });
            VerifyLoopCommandReport {
                attempted: true,
                command,
                status,
                exit_code: output.status.code(),
                duration_ms,
                summary,
                stdout_tail,
                stderr_tail,
            }
        }
        Err(err) => VerifyLoopCommandReport {
            attempted: true,
            command,
            status: VerifyLoopStepStatus::Fail,
            exit_code: None,
            duration_ms,
            summary: format!("failed to run cargo test: {err}"),
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        },
    }
}

fn is_aethyme_source_checkout(root: &Path) -> bool {
    root.join("packages/aethyme/rust/crates/aethyme-broker/Cargo.toml")
        .is_file()
        && root
            .join("packages/aethyme/rust/crates/aethyme-engine/Cargo.toml")
            .is_file()
}

fn tail_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > limit {
        lines = lines.split_off(lines.len() - limit);
    }
    lines
}

fn first_non_empty(lines: &[String]) -> Option<String> {
    lines.iter().find(|line| !line.trim().is_empty()).cloned()
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VersionDriftStatus;

    #[test]
    fn tail_lines_keeps_last_non_empty_lines() {
        assert_eq!(
            tail_lines("a\n\nb\nc\nd\n", 2),
            vec!["c".to_string(), "d".to_string()]
        );
    }

    #[test]
    fn skipped_source_tests_are_ok_for_non_source_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let report = run_source_tests(tmp.path());
        assert_eq!(report.status, VerifyLoopStepStatus::Skip);
        assert!(!report.attempted);
        assert!(report.status.is_ok());
    }

    #[test]
    fn integration_move_message_names_old_and_current_tips() {
        let tested = "aaaaaaaaaaaa0000000000000000000000000000";
        let current = "bbbbbbbbbbbb0000000000000000000000000000";
        let detail = format!(
            "tested old tip {}; current tip {}; rerun needed",
            short_commit(tested),
            short_commit(current)
        );
        assert_eq!(
            detail,
            "tested old tip aaaaaaaaaaaa; current tip bbbbbbbbbbbb; rerun needed"
        );
    }

    #[test]
    fn doctor_drift_status_is_a_failure_signal() {
        assert!(VersionDriftStatus::BehindIntegration.is_drift());
    }
}
