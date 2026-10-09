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
//! The capture's base is the merge base of the session head and the
//! integration tip at submit time, and its result is the session head. The
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

/// Capture prepared for one submit: the policy and either the exact request
/// or the report explaining why there is none.
pub(crate) struct SubmitCapture {
    policy: &'static str,
    required: bool,
    prepared: Result<(ProjectKey, CaptureRequest), SubmitCaptureReport>,
}

impl SubmitCapture {
    /// `None` when capture is off. Reads the session's commits now, before
    /// the submit can move the integration tip.
    pub(crate) fn prepare(broker: &mut crate::Broker, session: i64) -> Option<Self> {
        let (policy, project) = match setting(broker.main_root()) {
            Setting::Off => return None,
            Setting::Unsupported { value } => {
                return Some(Self {
                    policy: "unsupported",
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
        let required = policy == CapturePolicy::Required;
        let prepared = match project {
            Err(detail) => Err(SubmitCaptureReport::new(name, CaptureStatus::NotConfigured)
                .problem(
                    "no_project",
                    detail,
                    "set [collaboration] project = \"<key>\" in .aethyme/config.toml \
                     (1-64 lowercase letters, digits and '-'), or capture = \"off\"",
                )),
            Ok(project) => match request(broker, session, policy) {
                Ok(request) => Ok((project, request)),
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
            policy: name,
            required,
            prepared,
        })
    }

    pub(crate) fn required(&self) -> bool {
        self.required
    }

    /// Run the capture. Never panics and never returns an error: every
    /// outcome is a report.
    pub(crate) fn run(&self, main_root: &Path) -> SubmitCaptureReport {
        let (project, request) = match &self.prepared {
            Ok(prepared) => prepared,
            Err(report) => return report.clone(),
        };
        let mut report = SubmitCaptureReport::new(self.policy, CaptureStatus::Failed);
        report.operation_id = Some(request.operation_id.as_str().to_string());
        report.base_commit = Some(request.base.as_str().to_string());
        report.result_commit = Some(request.result.as_str().to_string());
        let mut store = match open_for_repository(main_root, project) {
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
        match capture(&mut store, request) {
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

/// The exact capture request for `session`'s current head.
fn request(
    broker: &mut crate::Broker,
    session: i64,
    policy: CapturePolicy,
) -> Result<CaptureRequest, String> {
    let info = broker
        .store()
        .session(session)
        .map_err(|error| error.to_string())?;
    let worktree = std::path::PathBuf::from(&info.worktree_path);
    let checkout = crate::GitRepo::discover(&worktree).map_err(|error| error.to_string())?;
    let result = checkout.head_commit().map_err(|error| error.to_string())?;
    let (_, integration) = broker
        .integration_head_snapshot()
        .map_err(|error| error.to_string())?;
    let base = checkout
        .merge_base(&integration, &result)
        .map_err(|error| error.to_string())?;
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
        repository: worktree,
        base: CommitOid::parse(&base).map_err(|error| error.to_string())?,
        result: CommitOid::parse(&result).map_err(|error| error.to_string())?,
        policy,
        retention: RetentionBoundary::UntilReleased,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
