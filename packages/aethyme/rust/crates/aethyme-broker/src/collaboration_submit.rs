//! Opt-in contribution capture at the submit boundary (#660, plan §6.4,
//! §6.7-6.8; D18, D32).
//!
//! Configured in `.aethyme/config.toml`, under the same trust rule as
//! `[promote]` (the copy committed on the fetched default branch wins):
//!
//! ```toml
//! [collaboration]
//! capture = "advisory"   # "off" (the default), "advisory" or "required"
//! project = "proj-7k2m"  # the project's collaboration directory key
//! ```
//!
//! - **Off** (no section, no `capture`, or `"off"`): nothing here runs, and
//!   submit is byte-for-byte the legacy command.
//! - **Advisory:** after the legacy submit has decided, the session's exact
//!   base and result are captured. The outcome is reported beside the legacy
//!   one and never changes its verdict, output or exit code.
//! - **Required:** the capture runs first. If it is not acknowledged the
//!   submit is refused before any queue entry, gate or promotion, so nothing
//!   reaches `aethyme/integration` without retained source.
//! - **Any other value** is refused as `unsupported_policy`, as if required:
//!   a newer policy this binary does not implement must never be treated as
//!   satisfied. Binaries that predate this module ignore the section
//!   entirely; see `local-v3-l2-optin.md`.
//!
//! The capture's base is the merge base of the submitted head and the
//! integration tip read before the submit, and its result is the submitted
//! head; see `submit` for how the two are bound to one commit. The
//! operation ID is derived from the session, both commits and the policy,
//! so a retried submit answers with the same receipt.

use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::collaboration_archive::CommitOid;
use crate::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureReceipt, CaptureRequest, OperationId, RetentionBoundary,
    capture,
};
use crate::collaboration_state::{ProjectKey, open_for_repository};

/// The schema of the `collaboration_capture` report in `submit --json`.
pub const SUBMIT_CAPTURE_SCHEMA: &str = "aethyme.submit-capture/experimental-v0";

/// What `[collaboration] capture` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Setting {
    Off,
    On {
        policy: CapturePolicy,
        project: Result<ProjectKey, String>,
    },
    Unsupported {
        value: String,
    },
}

pub(crate) fn setting(main_root: &Path) -> Setting {
    setting_from_text(crate::merge::repository_config_text(main_root).as_deref())
}

fn setting_from_text(text: Option<&str>) -> Setting {
    // An unreadable file is the legacy behaviour everywhere else in the
    // broker; only an explicit opt-in turns capture on.
    let Some(table) = text
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| value.get("collaboration").cloned())
    else {
        return Setting::Off;
    };
    let policy = match table.get("capture") {
        None => return Setting::Off,
        Some(toml::Value::String(value)) => match value.as_str() {
            "off" => return Setting::Off,
            "advisory" => CapturePolicy::Advisory,
            "required" => CapturePolicy::Required,
            other => {
                return Setting::Unsupported {
                    value: other.to_string(),
                };
            }
        },
        Some(other) => {
            return Setting::Unsupported {
                value: other.to_string(),
            };
        }
    };
    let project = match table.get("project") {
        Some(toml::Value::String(key)) => ProjectKey::parse(key).map_err(|error| error.to_string()),
        Some(other) => Err(format!("project must be a string, not {other}")),
        None => Err("no project is configured".into()),
    };
    Setting::On { policy, project }
}

/// The capture outcome reported beside a submit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubmitCaptureReport {
    pub schema: &'static str,
    /// `advisory`, `required`, or `unsupported`.
    pub policy: &'static str,
    pub status: CaptureStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ReceiptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Retained locally; `receipt` says under which durability profile.
    Acknowledged,
    /// The source was not all available; nothing was promised.
    Incomplete,
    /// The source cannot be retained as it is.
    Refused,
    /// Capture did not run or did not finish, for a local reason.
    Failed,
    /// Capture is enabled but cannot run with this configuration.
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiptReport {
    pub status: &'static str,
    pub durability: String,
    pub contribution: String,
    pub base_snapshot: String,
    pub result_snapshot: String,
    pub retention: String,
    pub receipt_record: String,
}

impl From<&CaptureReceipt> for ReceiptReport {
    fn from(receipt: &CaptureReceipt) -> Self {
        Self {
            status: receipt.status,
            durability: receipt.durability.clone(),
            contribution: receipt.contribution.as_str().to_string(),
            base_snapshot: receipt.base.as_str().to_string(),
            result_snapshot: receipt.result.as_str().to_string(),
            retention: receipt.retention.describe(),
            receipt_record: receipt.receipt_record_id.as_str().to_string(),
        }
    }
}

impl SubmitCaptureReport {
    pub fn acknowledged(&self) -> bool {
        self.status == CaptureStatus::Acknowledged
    }

    fn new(policy: &'static str, status: CaptureStatus) -> Self {
        Self {
            schema: SUBMIT_CAPTURE_SCHEMA,
            policy,
            status,
            operation_id: None,
            base_commit: None,
            result_commit: None,
            receipt: None,
            code: None,
            detail: None,
            next_action: None,
        }
    }

    fn problem(mut self, code: &str, detail: String, next_action: &str) -> Self {
        self.code = Some(code.to_string());
        self.detail = Some(detail);
        self.next_action = Some(next_action.to_string());
        self
    }

    /// One line for the human output.
    pub fn summary(&self) -> String {
        let head = format!("collaboration capture ({})", self.policy);
        match (&self.receipt, &self.code) {
            (Some(receipt), _) => format!(
                "{head}: retained_local ({}), contribution {}",
                receipt.durability, receipt.contribution
            ),
            (None, Some(code)) => format!(
                "{head}: {} [{code}] {}{}",
                serde_json::to_value(self.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_default(),
                self.detail.as_deref().unwrap_or(""),
                self.next_action
                    .as_deref()
                    .map(|next| format!(" — next: {next}"))
                    .unwrap_or_default()
            ),
            (None, None) => head,
        }
    }
}

/// `submit --json` with capture enabled: the legacy outcome's fields,
/// unchanged and in order, then `collaboration_capture`.
#[derive(Serialize)]
pub(crate) struct WithCapture<'a, T: Serialize> {
    #[serde(flatten)]
    pub outcome: &'a T,
    pub collaboration_capture: &'a SubmitCaptureReport,
}

/// What a submit with collaboration enabled produced.
pub(crate) enum CaptureSubmit {
    /// Required capture was not acknowledged; nothing was submitted.
    Refused(SubmitCaptureReport),
    /// The legacy submit ran; `capture` is `None` when capture is off.
    Submitted {
        outcome: Box<crate::SubmitOutcome>,
        capture: Option<SubmitCaptureReport>,
    },
}

type OpenStore<'a> = dyn FnMut(
        &Path,
        &ProjectKey,
    ) -> Result<
        crate::collaboration_state::CollaborationStore,
        crate::collaboration_state::CollaborationStateError,
    > + 'a;

/// Submit `session`, capturing it as `[collaboration]` asks.
///
/// The capture and the submit are bound to one commit:
/// - **Required:** the captured head is passed to
///   [`crate::Broker::submit_expecting_head`], which refuses with
///   `CapturedHeadMoved` before any queue entry if the session moved after
///   the capture. A receipt therefore always names the submitted commit.
/// - **Advisory:** the capture runs after the submit, on the head the submit
///   pinned into its queue entry, never on an earlier reading.
pub(crate) fn submit(
    broker: &mut crate::Broker,
    session: i64,
    cache: crate::CachePolicy,
    intent: crate::PromotionIntent,
) -> Result<CaptureSubmit, crate::BrokerOpError> {
    submit_with(
        broker,
        session,
        cache,
        intent,
        &mut |root, project| open_for_repository(root, project),
        &mut || {},
    )
}

/// [`submit`] with the store opener injected, and `between` run after the
/// capture inputs are read and before the submit pins its head, so a test
/// can move the session there.
fn submit_with(
    broker: &mut crate::Broker,
    session: i64,
    cache: crate::CachePolicy,
    intent: crate::PromotionIntent,
    open: &mut OpenStore<'_>,
    between: &mut dyn FnMut(),
) -> Result<CaptureSubmit, crate::BrokerOpError> {
    let Some(capture) = SubmitCapture::prepare(broker, session) else {
        let outcome = broker.submit_with_intent(session, cache, intent)?;
        return Ok(CaptureSubmit::Submitted {
            outcome: Box::new(outcome),
            capture: None,
        });
    };
    if capture.required {
        let report = capture.run(broker.main_root(), None, open);
        let Some(head) = report
            .acknowledged()
            .then(|| report.result_commit.clone())
            .flatten()
        else {
            return Ok(CaptureSubmit::Refused(report));
        };
        between();
        let outcome = broker.submit_expecting_head(session, cache, intent, &head)?;
        return Ok(CaptureSubmit::Submitted {
            outcome: Box::new(outcome),
            capture: Some(report),
        });
    }
    between();
    let outcome = broker.submit_with_intent(session, cache, intent)?;
    let report = capture.run(broker.main_root(), Some(&outcome.entry.head_commit), open);
    Ok(CaptureSubmit::Submitted {
        outcome: Box::new(outcome),
        capture: Some(report),
    })
}

/// The commits a capture is computed from, read before the submit can move
/// the integration tip.
struct Prepared {
    project: ProjectKey,
    session: i64,
    worktree: std::path::PathBuf,
    integration: String,
    head: String,
}

/// Capture prepared for one submit: the policy and either its inputs or the
/// report explaining why there are none.
struct SubmitCapture {
    policy: CapturePolicy,
    name: &'static str,
    required: bool,
    prepared: Result<Prepared, SubmitCaptureReport>,
}

impl SubmitCapture {
    /// `None` when capture is off.
    fn prepare(broker: &mut crate::Broker, session: i64) -> Option<Self> {
        let (policy, project) = match setting(broker.main_root()) {
            Setting::Off => return None,
            Setting::Unsupported { value } => {
                return Some(Self {
                    policy: CapturePolicy::Required,
                    name: "unsupported",
                    required: true,
                    prepared: Err(SubmitCaptureReport::new(
                        "unsupported",
                        CaptureStatus::NotConfigured,
                    )
                    .problem(
                        "unsupported_policy",
                        format!(
                            "[collaboration] capture = {value} is not a policy this binary \
                             implements, so it is treated as required and not satisfied"
                        ),
                        "use capture = \"off\", \"advisory\" or \"required\", or upgrade Aethyme",
                    )),
                });
            }
            Setting::On { policy, project } => (policy, project),
        };
        let name = policy.as_str();
        let prepared = match project {
            Err(detail) => Err(SubmitCaptureReport::new(name, CaptureStatus::NotConfigured)
                .problem(
                    "no_project",
                    detail,
                    "set [collaboration] project = \"<key>\" in .aethyme/config.toml \
                     (1-64 lowercase letters, digits and '-'), or capture = \"off\"",
                )),
            Ok(project) => match inputs(broker, session, project) {
                Ok(prepared) => Ok(prepared),
                Err(detail) => Err(
                    SubmitCaptureReport::new(name, CaptureStatus::Failed).problem(
                        "inputs_unavailable",
                        detail,
                        "check the session worktree and the integration branch, then resubmit",
                    ),
                ),
            },
        };
        Some(Self {
            policy,
            name,
            required: policy == CapturePolicy::Required,
            prepared,
        })
    }

    /// Capture `result` (default: the head read at preparation). Never
    /// panics and never returns an error: every outcome is a report.
    fn run(
        &self,
        main_root: &Path,
        result: Option<&str>,
        open: &mut OpenStore<'_>,
    ) -> SubmitCaptureReport {
        let prepared = match &self.prepared {
            Ok(prepared) => prepared,
            Err(report) => return report.clone(),
        };
        let request = match request(prepared, result.unwrap_or(&prepared.head), self.policy) {
            Ok(request) => request,
            Err(detail) => {
                return SubmitCaptureReport::new(self.name, CaptureStatus::Failed).problem(
                    "inputs_unavailable",
                    detail,
                    "check the session worktree and the integration branch, then resubmit",
                );
            }
        };
        let mut report = SubmitCaptureReport::new(self.name, CaptureStatus::Failed);
        report.operation_id = Some(request.operation_id.as_str().to_string());
        report.base_commit = Some(request.base.as_str().to_string());
        report.result_commit = Some(request.result.as_str().to_string());
        let mut store = match open(main_root, &prepared.project) {
            Ok(store) => store,
            Err(error) => {
                report.status = CaptureStatus::NotConfigured;
                return report.problem(
                    error.code(),
                    error.to_string(),
                    "fix the collaboration state location named in the detail, then resubmit",
                );
            }
        };
        match capture(&mut store, &request) {
            Ok(CaptureOutcome::Acknowledged(receipt)) => {
                report.status = CaptureStatus::Acknowledged;
                report.receipt = Some(ReceiptReport::from(&receipt));
                report
            }
            Ok(CaptureOutcome::Incomplete { code, detail, .. }) => {
                report.status = CaptureStatus::Incomplete;
                report.problem(
                    &code,
                    detail,
                    "make the full history and every object available (for example \
                     `git fetch --unshallow`), then resubmit; the same operation resumes",
                )
            }
            Err(error) => {
                report.status = if error.code() == "refused" {
                    CaptureStatus::Refused
                } else {
                    CaptureStatus::Failed
                };
                let next = if report.status == CaptureStatus::Refused {
                    "the source cannot be retained as it is (see detail); change it or set \
                     capture = \"off\" for this repository"
                } else {
                    "see the detail; resubmitting retries the same operation"
                };
                report.problem(error.code(), error.to_string(), next)
            }
        }
    }
}

/// Read the session's worktree, head and the integration tip.
fn inputs(
    broker: &mut crate::Broker,
    session: i64,
    project: ProjectKey,
) -> Result<Prepared, String> {
    let info = broker
        .store()
        .session(session)
        .map_err(|error| error.to_string())?;
    let worktree = std::path::PathBuf::from(&info.worktree_path);
    let checkout = crate::GitRepo::discover(&worktree).map_err(|error| error.to_string())?;
    let head = checkout.head_commit().map_err(|error| error.to_string())?;
    let (_, integration) = broker
        .integration_head_snapshot()
        .map_err(|error| error.to_string())?;
    Ok(Prepared {
        project,
        session,
        worktree,
        integration,
        head,
    })
}

/// The exact capture request for `result`: its base is the merge base with
/// the integration tip read at preparation.
fn request(
    prepared: &Prepared,
    result: &str,
    policy: CapturePolicy,
) -> Result<CaptureRequest, String> {
    let checkout =
        crate::GitRepo::discover(&prepared.worktree).map_err(|error| error.to_string())?;
    let base = checkout
        .merge_base(&prepared.integration, result)
        .map_err(|error| error.to_string())?;
    let session = prepared.session;
    let digest: String = Sha256::digest(
        format!(
            "aethyme submit capture v0\0{session}\0{base}\0{result}\0{}",
            policy.as_str()
        )
        .as_bytes(),
    )
    .iter()
    .take(20)
    .map(|byte| format!("{byte:02x}"))
    .collect();
    Ok(CaptureRequest {
        operation_id: OperationId::parse(&format!("submit:{digest}"))
            .map_err(|error| error.to_string())?,
        repository: prepared.worktree.clone(),
        base: CommitOid::parse(&base).map_err(|error| error.to_string())?,
        result: CommitOid::parse(result).map_err(|error| error.to_string())?,
        policy,
        retention: RetentionBoundary::UntilReleased,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::collaboration_state::{CollaborationRoot, CollaborationStore};
    use std::process::Command;

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
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct Fixture {
        repo: tempfile::TempDir,
        _worktrees: tempfile::TempDir,
        state: tempfile::TempDir,
        broker: crate::Broker,
        session: i64,
        worktree: std::path::PathBuf,
    }

    /// A repository with `policy` capture and one committed session.
    fn fixture(policy: &str) -> Fixture {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "init"]);
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(".aethyme/config.toml"),
            format!("[collaboration]\ncapture = \"{policy}\"\nproject = \"proj-test\"\n"),
        )
        .unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let mut broker = crate::Broker::open(repo.path())
            .unwrap()
            .with_worktree_root(worktrees.path());
        let session = broker.start_worktree("work", None).unwrap();
        let worktree = std::path::PathBuf::from(&session.worktree_path);
        commit(&worktree, "first");
        Fixture {
            repo,
            _worktrees: worktrees,
            state: tempfile::tempdir().unwrap(),
            broker,
            session: session.id,
            worktree,
        }
    }

    fn commit(worktree: &Path, name: &str) -> String {
        std::fs::write(worktree.join(format!("{name}.txt")), "payload\n").unwrap();
        git(worktree, &["add", "-A"]);
        git(worktree, &["commit", "-qm", name]);
        git(worktree, &["rev-parse", "HEAD"])
    }

    /// Submit with the session moved by a new commit between the capture
    /// inputs and the submit.
    fn submit_after_a_move(
        fixture: &mut Fixture,
    ) -> (Result<CaptureSubmit, crate::BrokerOpError>, String) {
        let state = fixture.state.path().to_path_buf();
        let worktree = fixture.worktree.clone();
        let mut moved = String::new();
        let result = submit_with(
            &mut fixture.broker,
            fixture.session,
            crate::CachePolicy::Use,
            crate::PromotionIntent::Configured,
            &mut |_, project| {
                CollaborationStore::open(&CollaborationRoot::under_host_state(&state), project, &[])
            },
            &mut || moved = commit(&worktree, "second"),
        );
        (result, moved)
    }

    /// Required: the captured commit is the only one that may be submitted.
    /// A move after the capture is refused before any queue entry, and the
    /// integration branch does not move.
    #[test]
    fn a_session_moved_after_a_required_capture_is_not_submitted() {
        let mut fixture = fixture("required");
        let integration = git(fixture.repo.path(), &["for-each-ref", "refs/heads/aethyme"]);
        let (result, moved) = submit_after_a_move(&mut fixture);
        match result {
            Err(crate::BrokerOpError::CapturedHeadMoved { actual, .. }) => {
                assert_eq!(&*actual, moved.as_str());
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("submitted a commit that was never captured"),
        }
        assert_eq!(
            crate::exit_status::for_broker_error(&crate::BrokerOpError::CapturedHeadMoved {
                session_id: 0,
                captured: "a".into(),
                actual: "b".into(),
            }),
            crate::exit_status::REFUSED
        );
        assert!(fixture.broker.store().merge_queue().unwrap().is_empty());
        assert_eq!(
            git(fixture.repo.path(), &["for-each-ref", "refs/heads/aethyme"]),
            integration
        );
    }

    /// Advisory: the receipt names the commit the submit actually pinned.
    #[test]
    fn an_advisory_capture_retains_the_commit_that_was_submitted() {
        let mut fixture = fixture("advisory");
        let (result, moved) = submit_after_a_move(&mut fixture);
        let Ok(CaptureSubmit::Submitted {
            outcome,
            capture: Some(report),
        }) = result
        else {
            panic!("advisory submit did not report a capture");
        };
        assert_eq!(outcome.entry.head_commit, moved);
        assert_eq!(report.status, CaptureStatus::Acknowledged, "{report:?}");
        assert_eq!(report.result_commit.as_deref(), Some(moved.as_str()));
    }

    /// Without a move, a required capture submits the captured commit.
    #[test]
    fn a_required_capture_submits_the_captured_commit() {
        let mut fixture = fixture("required");
        let state = fixture.state.path().to_path_buf();
        let Ok(CaptureSubmit::Submitted {
            outcome,
            capture: Some(report),
        }) = submit_with(
            &mut fixture.broker,
            fixture.session,
            crate::CachePolicy::Use,
            crate::PromotionIntent::Configured,
            &mut |_, project| {
                CollaborationStore::open(&CollaborationRoot::under_host_state(&state), project, &[])
            },
            &mut || {},
        )
        else {
            panic!("required submit failed");
        };
        assert_eq!(
            report.result_commit.as_deref(),
            Some(outcome.entry.head_commit.as_str())
        );
        assert!(outcome.promoted);
    }

    #[test]
    fn only_an_explicit_known_value_turns_capture_on() {
        for text in [
            None,
            Some(""),
            Some("not toml ["),
            Some("[promote]\nmode = \"auto\"\n"),
            Some("[collaboration]\nproject = \"p\"\n"),
            Some("[collaboration]\ncapture = \"off\"\nproject = \"p\"\n"),
        ] {
            assert_eq!(setting_from_text(text), Setting::Off, "{text:?}");
        }
        assert_eq!(
            setting_from_text(Some(
                "[collaboration]\ncapture = \"advisory\"\nproject = \"p\"\n"
            )),
            Setting::On {
                policy: CapturePolicy::Advisory,
                project: Ok(ProjectKey::parse("p").unwrap()),
            }
        );
        assert!(matches!(
            setting_from_text(Some("[collaboration]\ncapture = \"required\"\n")),
            Setting::On {
                policy: CapturePolicy::Required,
                project: Err(_),
            }
        ));
    }

    /// A value this binary does not implement, including a newer policy or a
    /// typo, is never treated as off.
    #[test]
    fn an_unknown_policy_is_unsupported_not_off() {
        for value in ["\"requried\"", "\"required-v2\"", "true", "1"] {
            let text = format!("[collaboration]\ncapture = {value}\nproject = \"p\"\n");
            assert!(
                matches!(setting_from_text(Some(&text)), Setting::Unsupported { .. }),
                "{value}"
            );
        }
    }

    #[test]
    fn an_invalid_project_key_is_reported() {
        let setting = setting_from_text(Some(
            "[collaboration]\ncapture = \"advisory\"\nproject = \"Not A Key\"\n",
        ));
        assert!(matches!(
            setting,
            Setting::On {
                project: Err(_),
                ..
            }
        ));
    }
}
