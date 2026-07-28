//! Built-in broker smoke test for first-run confidence checks.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Broker, BrokerOpError};

const CHAU7_SKIP_MESSAGE: &str = "this test is designed to run in Chau7";

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

#[derive(Debug, serde::Serialize)]
pub struct QuickTestReport {
    pub ok: bool,
    pub skipped: bool,
    pub mode: QuickTestMode,
    pub message: String,
    pub temp_repo: String,
    pub temp_repo_removed: bool,
    pub chau7: Chau7Probe,
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub integration_head: Option<String>,
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
    #[error("broker submit did not promote the smoke commit")]
    SubmitNotPromoted,
}

pub fn run_broker_quick_test(mode: QuickTestMode) -> Result<QuickTestReport, QuickTestError> {
    let chau7 = detect_chau7_environment();
    if mode == QuickTestMode::Chau7 && !chau7.detected {
        return Ok(QuickTestReport {
            ok: false,
            skipped: true,
            mode,
            message: CHAU7_SKIP_MESSAGE.into(),
            temp_repo: String::new(),
            temp_repo_removed: false,
            chau7,
            session_id: None,
            queue_entry_id: None,
            integration_head: None,
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

    let init = crate::init::guided_init(temp_repo.path())?;
    if !init.certified() {
        return Err(QuickTestError::InitNotCertified);
    }
    git(temp_repo.path(), &["add", "-A"])?;
    git(
        temp_repo.path(),
        &["commit", "-q", "-m", "chore: initialize aethyme broker"],
    )?;
    steps.push(step("aethyme-init", "pass", "scaffold committed"));

    let mut broker = Broker::open(temp_repo.path())?;
    let session = broker.adopt(temp_repo.path(), Some("broker quick test"))?;
    steps.push(step(
        "broker-adopt",
        "pass",
        format!("session {}", session.id),
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
        return Err(QuickTestError::SubmitNotPromoted);
    }
    let integration_head = broker.integration_head()?.1;
    steps.push(step(
        "broker-submit",
        "pass",
        format!("entry {} promoted", outcome.entry.id),
    ));

    let temp_path = temp_repo.path().display().to_string();
    temp_repo.remove()?;

    Ok(QuickTestReport {
        ok: true,
        skipped: false,
        mode,
        message: "broker quick test passed".into(),
        temp_repo: temp_path,
        temp_repo_removed: true,
        chau7,
        session_id: Some(session.id),
        queue_entry_id: Some(outcome.entry.id),
        integration_head: Some(integration_head),
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

fn git(cwd: &Path, args: &[&str]) -> Result<String, QuickTestError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
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
    remove_on_drop: bool,
}

impl TempRepo {
    fn create() -> Result<Self, std::io::Error> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aethyme-broker-quick-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self {
            path,
            remove_on_drop: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(mut self) -> Result<(), std::io::Error> {
        std::fs::remove_dir_all(&self.path)?;
        self.remove_on_drop = false;
        Ok(())
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
        assert!(report.temp_repo_removed);
        assert!(report.session_id.is_some());
        assert!(report.queue_entry_id.is_some());
        assert!(report.integration_head.is_some());
        assert!(report.steps.iter().any(|step| step.name == "broker-submit"));
    }
}
