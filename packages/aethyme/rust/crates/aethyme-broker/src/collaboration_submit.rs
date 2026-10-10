//! Opt-in contribution capture at the submit boundary (#660, plan §6.4,
//! §6.7-6.8; D18, D32).
//!
//! Configured in `.aethyme/config.toml`, read with the same rule as
//! `[promote]`: the copy committed on the fetched default branch when there
//! is one and it holds the file, otherwise the main checkout's working copy.
//! The report names which (`config_source`).
//!
//! ```toml
//! [collaboration]
//! capture = "advisory"   # "off" (the default), "advisory" or "required"
//! project = "proj-7k2m"  # the project's collaboration directory key
//! ```
//!
//! - **Off** (no section, no `capture`, or `"off"`): nothing here runs past
//!   reading the config, and submit is the legacy command.
//! - **Advisory:** after the legacy submit has decided (and, in text mode,
//!   after its verdict is printed), the submitted commit is captured. The
//!   outcome is reported beside the legacy one. It cannot change the verdict,
//!   output or exit code: it never returns an error, catches panics, and
//!   reports `in_progress` instead of waiting for another capture's lock.
//! - **Required:** the capture runs first, on the base the submit will verify
//!   against. If it is not acknowledged the submit is refused before any
//!   queue entry. The submit is bound to the captured commit, and every
//!   promotion (automatic, `promote --entry`, re-verification, queue drain)
//!   refuses a head without an acknowledged required capture.
//! - **Anything else** fails closed: an unknown `capture` value, a
//!   `collaboration` key that is not a table, or an unreadable config that
//!   visibly mentions `collaboration`. A repository whose config does not
//!   mention it stays the legacy submit even when the file is malformed.
//!   Binaries that predate this module ignore the section; see
//!   `local-v3-l2-optin.md`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::collaboration_archive::CommitOid;
use crate::collaboration_capture::{
    CaptureOutcome, CapturePolicy, CaptureReceipt, CaptureRequest, OperationId, RetentionBoundary,
    capture, try_capture,
};
use crate::collaboration_state::{CollaborationStateError, CollaborationStore, ProjectKey};

/// The schema of the `collaboration_capture` report in `submit --json`.
pub const SUBMIT_CAPTURE_SCHEMA: &str = "aethyme.submit-capture/experimental-v0";

/// The `[collaboration]` settings this binary implements.
const KNOWN_SETTINGS: &[&str] = &["capture", "project"];

/// What `[collaboration] capture` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Setting {
    Off,
    On {
        policy: CapturePolicy,
        project: Result<ProjectKey, String>,
    },
    /// Fails closed, as a required capture that cannot be satisfied.
    Unsupported {
        code: &'static str,
        detail: String,
    },
}

/// The setting and where the config came from (`None` without a config).
pub(crate) fn setting(main_root: &Path) -> (Setting, Option<&'static str>) {
    match crate::merge::repository_config_with_source(main_root) {
        Some((text, source)) => (setting_from_text(Some(&text)), Some(source)),
        None => (Setting::Off, None),
    }
}

/// Whether the raw text visibly opts in, read without parsing it: a
/// `[collaboration...]` header or a `collaboration` key.
fn mentions_collaboration(text: &str) -> bool {
    text.lines().map(str::trim_start).any(|line| {
        line.strip_prefix('[')
            .map(|rest| rest.trim_start().starts_with("collaboration"))
            .unwrap_or(false)
            || line.strip_prefix("collaboration").is_some_and(|rest| {
                let rest = rest.trim_start();
                rest.starts_with('=') || rest.starts_with('.')
            })
    })
}

/// Whether `setting` is an explicit `capture = "required"`, the only setting
/// that raises `broker.db`'s compatibility floor.
pub(crate) fn requires_capture(setting: &Setting) -> bool {
    matches!(
        setting,
        Setting::On {
            policy: CapturePolicy::Required,
            ..
        }
    )
}

pub(crate) fn setting_from_text(text: Option<&str>) -> Setting {
    let Some(text) = text else {
        return Setting::Off;
    };
    let value = match text.parse::<toml::Value>() {
        Ok(value) => value,
        // An unreadable file stays the legacy behaviour, unless it visibly
        // opted in: a broken opt-in must not silently turn capture off.
        Err(error) if mentions_collaboration(text) => {
            return Setting::Unsupported {
                code: "config_unreadable",
                detail: format!(
                    ".aethyme/config.toml mentions [collaboration] but is not valid TOML \
                     ({}), so the capture policy cannot be read",
                    error.message()
                ),
            };
        }
        Err(_) => return Setting::Off,
    };
    let table = match value.get("collaboration") {
        None => return Setting::Off,
        Some(toml::Value::Table(table)) => table,
        Some(other) => {
            return Setting::Unsupported {
                code: "unsupported_policy",
                detail: format!(
                    "collaboration is a {}, not a [collaboration] table",
                    other.type_str()
                ),
            };
        }
    };
    let policy = match table.get("capture") {
        None => return Setting::Off,
        Some(toml::Value::String(value)) => match value.as_str() {
            "off" => return Setting::Off,
            "advisory" => CapturePolicy::Advisory,
            "required" => CapturePolicy::Required,
            other => {
                return Setting::Unsupported {
                    code: "unsupported_policy",
                    detail: format!(
                        "[collaboration] capture = {other:?} is not a policy this binary \
                         implements, so it is treated as required and not satisfied"
                    ),
                };
            }
        },
        Some(other) => {
            return Setting::Unsupported {
                code: "unsupported_policy",
                detail: format!(
                    "[collaboration] capture = {other} is not a policy this binary implements, \
                     so it is treated as required and not satisfied"
                ),
            };
        }
    };
    // Every [collaboration] setting is critical: a key this binary does not
    // implement may change what the policy means, so an enabled policy
    // carrying one is refused rather than half-applied (§6.7). A repository
    // that has not enabled capture is unaffected.
    let mut unknown: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|key| !KNOWN_SETTINGS.contains(key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Setting::Unsupported {
            code: "unknown_setting",
            detail: format!(
                "[collaboration] has {} this binary does not implement ({}); only {} are \
                 understood, so the policy is treated as required and not satisfied",
                if unknown.len() == 1 {
                    "a setting"
                } else {
                    "settings"
                },
                unknown.join(", "),
                KNOWN_SETTINGS.join(" and ")
            ),
        };
    }
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
    /// `committed` (fetched default branch) or `working_copy`.
    pub config_source: &'static str,
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
    /// Path-free: host paths are replaced with placeholders such as
    /// `<host state>` or `<path>`.
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
    /// Another process is capturing the same operation; advisory capture
    /// does not wait for it.
    InProgress,
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

    fn new(policy: &'static str, config_source: &'static str, status: CaptureStatus) -> Self {
        Self {
            schema: SUBMIT_CAPTURE_SCHEMA,
            policy,
            config_source,
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

    /// Replace host paths in `detail` with placeholders.
    fn without_paths(mut self, known: &[(PathBuf, &str)]) -> Self {
        self.detail = self.detail.map(|detail| path_free(&detail, known));
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

/// Host paths a detail may mention, most specific first, each with the
/// placeholder that replaces it. Both the given and the canonical spelling
/// are listed (`/var` and `/private/var` on macOS).
fn known_paths(main_root: &Path, worktree: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let mut known: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut add = |path: PathBuf, label: &'static str| {
        if let Ok(canonical) = path.canonicalize() {
            known.push((canonical, label));
        }
        known.push((path, label));
    };
    if let Some(worktree) = worktree {
        add(worktree.to_path_buf(), "<session worktree>");
    }
    add(main_root.to_path_buf(), "<repository>");
    let (states, caches) = crate::host_state::host_directory_candidates();
    for state in states {
        add(state, "<host state>");
    }
    for cache in caches {
        add(cache, "<host cache>");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        add(PathBuf::from(home), "<home>");
    }
    add(std::env::temp_dir(), "<temp>");
    // Longest first, so a nested root is replaced before its parent.
    known.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_os_str().len()));
    known
}

/// `detail` with every known root replaced by its placeholder, then any
/// remaining absolute path (a word starting with `/`, and the words that
/// continue it after a space) replaced by `<path>`.
fn path_free(detail: &str, known: &[(PathBuf, &str)]) -> String {
    let mut text = detail.to_string();
    for (path, label) in known {
        let path = path.to_string_lossy();
        if path.len() > 1 {
            text = text.replace(path.as_ref(), label);
        }
    }
    let mut out = Vec::new();
    let mut in_path = false;
    for word in text.split(' ') {
        let bare = word.trim_start_matches(['"', '\'', '(', '`']);
        if bare.starts_with('/') && bare.len() > 1 {
            out.push("<path>".to_string());
            in_path = true;
        } else if in_path && word.contains('/') {
            // The rest of a path containing a space ("Application Support/...").
        } else {
            out.push(word.to_string());
            in_path = false;
        }
    }
    out.join(" ")
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
    /// Required capture was acknowledged, then the session moved, so the
    /// submit was refused (`CapturedHeadMoved`) before any queue entry. The
    /// receipt retains a commit that was not submitted; it is harmless and
    /// answers again if that commit is ever submitted.
    HeadMoved {
        report: SubmitCaptureReport,
        error: crate::BrokerOpError,
    },
    /// The legacy submit ran.
    Submitted {
        outcome: Box<crate::SubmitOutcome>,
        capture: Option<CaptureStep>,
    },
}

/// The capture side of a submit that ran.
pub(crate) enum CaptureStep {
    /// Required: captured before the submit.
    Done(SubmitCaptureReport),
    /// Advisory: to run after the legacy verdict is shown.
    Pending(AdvisoryCapture),
}

impl CaptureStep {
    /// The report, running a pending advisory capture now.
    pub(crate) fn finish(self, broker: &crate::Broker) -> SubmitCaptureReport {
        match self {
            Self::Done(report) => report,
            Self::Pending(advisory) => advisory.run(broker),
        }
    }
}

type OpenStore<'a> =
    dyn FnMut(&ProjectKey) -> Result<CollaborationStore, CollaborationStateError> + 'a;

/// An advisory capture of the commit a submit pinned, against the base it
/// verified against.
pub(crate) struct AdvisoryCapture {
    capture: SubmitCapture,
    base: String,
    head: String,
}

impl AdvisoryCapture {
    pub(crate) fn run(self, broker: &crate::Broker) -> SubmitCaptureReport {
        self.run_with(broker.main_root(), &mut |project| {
            broker.collaboration_store(project)
        })
    }

    /// Never returns an error, never unwinds, never waits for another
    /// capture's lock.
    fn run_with(self, main_root: &Path, open: &mut OpenStore<'_>) -> SubmitCaptureReport {
        let name = self.capture.name;
        let source = self.capture.source;
        let worktree = self.capture.worktree().map(Path::to_path_buf);
        catch_unwind(AssertUnwindSafe(|| {
            self.capture.run(&self.base, &self.head, false, open)
        }))
        .unwrap_or_else(|_| {
            SubmitCaptureReport::new(name, source, CaptureStatus::Failed).problem(
                "panicked",
                "the advisory capture stopped unexpectedly; the legacy submit is unaffected".into(),
                "resubmit to retry the capture",
            )
        })
        .without_paths(&known_paths(main_root, worktree.as_deref()))
    }
}

/// Submit `session`, capturing it as `[collaboration]` asks.
///
/// The capture and the submit agree on one base and one commit:
/// - **Required:** integration is refreshed and the submission base read
///   first, exactly as submit does; that base and the head are captured;
///   then [`crate::Broker::submit_expecting_head`] refuses with
///   `CapturedHeadMoved` before any queue entry if the session moved.
/// - **Advisory:** nothing runs before the submit. The returned
///   [`CaptureStep::Pending`] captures the head the submit pinned against the
///   base it verified against.
pub(crate) fn submit(
    broker: &mut crate::Broker,
    session: i64,
    cache: crate::CachePolicy,
    intent: crate::PromotionIntent,
) -> Result<CaptureSubmit, crate::BrokerOpError> {
    submit_with(broker, session, cache, intent, None, &mut || {})
}

/// [`submit`] with the store opener injected, and `between` run after the
/// capture inputs are read and before the submit pins its head, so a test
/// can move the session there.
fn submit_with(
    broker: &mut crate::Broker,
    session: i64,
    cache: crate::CachePolicy,
    intent: crate::PromotionIntent,
    open: Option<&mut OpenStore<'_>>,
    between: &mut dyn FnMut(),
) -> Result<CaptureSubmit, crate::BrokerOpError> {
    let Some(capture) = SubmitCapture::prepare(broker, session) else {
        let outcome = broker.submit_with_intent(session, cache, intent)?;
        return Ok(CaptureSubmit::Submitted {
            outcome: Box::new(outcome),
            capture: None,
        });
    };
    if !capture.required {
        between();
        let outcome = broker.submit_with_intent(session, cache, intent)?;
        let base = outcome
            .verified_against
            .as_ref()
            .map(|base| base.commit.clone())
            .unwrap_or_else(|| outcome.entry.base_commit.clone());
        let head = outcome.entry.head_commit.clone();
        return Ok(CaptureSubmit::Submitted {
            outcome: Box::new(outcome),
            capture: Some(CaptureStep::Pending(AdvisoryCapture {
                capture,
                base,
                head,
            })),
        });
    }
    // Raise the compatibility floor before capturing, so no pre-#660 binary
    // can submit to this repository uncaptured from here on.
    let config = crate::merge::repository_config_with_source(broker.main_root());
    broker.collaboration_fence_state(crate::broker::FenceTrigger::Config(config.as_ref()));
    let main_root = broker.main_root().to_path_buf();
    let worktree = capture.worktree().map(Path::to_path_buf);
    let known = known_paths(&main_root, worktree.as_deref());
    let report = match &capture.prepared {
        Err(report) => report.clone(),
        Ok(prepared) => {
            // The base submit will verify against, read the way submit reads
            // it, so a resubmit after integration refreshes is the same
            // operation.
            broker.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Submit);
            let base = broker.submission_base()?.commit;
            let head = prepared.head.clone();
            match open {
                Some(open) => capture.run(&base, &head, true, open),
                None => capture.run(&base, &head, true, &mut |project| {
                    broker.collaboration_store(project)
                }),
            }
        }
    }
    .without_paths(&known);
    let Some(head) = report
        .acknowledged()
        .then(|| report.result_commit.clone())
        .flatten()
    else {
        return Ok(CaptureSubmit::Refused(report));
    };
    between();
    match broker.submit_expecting_head(session, cache, intent, &head) {
        Ok(outcome) => Ok(CaptureSubmit::Submitted {
            outcome: Box::new(outcome),
            capture: Some(CaptureStep::Done(report)),
        }),
        Err(error @ crate::BrokerOpError::CapturedHeadMoved { .. }) => {
            Ok(CaptureSubmit::HeadMoved { report, error })
        }
        Err(error) => Err(error),
    }
}

/// Refuse to promote `head` when the repository requires capture and no
/// acknowledged required capture of it exists. Off and advisory return
/// before touching anything but the config.
pub(crate) fn require_capture_for_promotion(
    broker: &crate::Broker,
    entry: i64,
    head: &str,
) -> Result<(), crate::BrokerOpError> {
    let refuse = |reason: String| crate::BrokerOpError::CaptureRequiredForPromotion {
        entry,
        head: head.into(),
        reason,
    };
    let project = match setting(broker.main_root()).0 {
        Setting::Off
        | Setting::On {
            policy: CapturePolicy::Advisory,
            ..
        } => return Ok(()),
        Setting::Unsupported { code, .. } => return Err(refuse(code.to_string())),
        Setting::On { project, .. } => project.map_err(|_| refuse("no_project".into()))?,
    };
    let config = crate::merge::repository_config_with_source(broker.main_root());
    broker.collaboration_fence_state(crate::broker::FenceTrigger::Config(config.as_ref()));
    let store = broker
        .collaboration_store(&project)
        .map_err(|e| refuse(e.code().into()))?;
    let found = store
        .read_connection()
        .query_row(
            "SELECT 1 FROM capture_operations o
             JOIN capture_receipts r ON r.operation_id = o.operation_id
             WHERE o.result_commit = ?1 AND o.policy = 'required'
               AND o.state IN ('committed', 'acknowledged')
             LIMIT 1",
            [head],
            |_| Ok(()),
        )
        .optional()
        .map_err(|_| refuse("state_unreadable".into()))?;
    found.ok_or_else(|| refuse("no_receipt".into()))
}

/// The session's worktree and head, read before the submit.
struct Prepared {
    project: ProjectKey,
    session: i64,
    worktree: PathBuf,
    head: String,
}

/// Capture prepared for one submit: the policy and either its inputs or the
/// report explaining why there are none.
struct SubmitCapture {
    policy: CapturePolicy,
    name: &'static str,
    source: &'static str,
    required: bool,
    prepared: Result<Prepared, SubmitCaptureReport>,
}

impl SubmitCapture {
    /// `None` when capture is off.
    fn prepare(broker: &mut crate::Broker, session: i64) -> Option<Self> {
        let (setting, source) = setting(broker.main_root());
        let source = source.unwrap_or("working_copy");
        let (policy, project) = match setting {
            Setting::Off => return None,
            Setting::Unsupported { code, detail } => {
                return Some(Self {
                    policy: CapturePolicy::Required,
                    name: "unsupported",
                    source,
                    required: true,
                    prepared: Err(SubmitCaptureReport::new(
                        "unsupported",
                        source,
                        CaptureStatus::NotConfigured,
                    )
                    .problem(
                        code,
                        detail,
                        "fix [collaboration] in .aethyme/config.toml: capture = \"off\", \
                         \"advisory\" or \"required\" (or upgrade Aethyme)",
                    )),
                });
            }
            Setting::On { policy, project } => (policy, project),
        };
        let name = policy.as_str();
        let prepared = match project {
            Err(detail) => {
                Err(
                    SubmitCaptureReport::new(name, source, CaptureStatus::NotConfigured).problem(
                        "no_project",
                        detail,
                        "set [collaboration] project = \"<key>\" in .aethyme/config.toml \
                     (1-64 lowercase letters, digits and '-'), or capture = \"off\"",
                    ),
                )
            }
            Ok(project) => match inputs(broker, session, project) {
                Ok(prepared) => Ok(prepared),
                Err(detail) => Err(
                    SubmitCaptureReport::new(name, source, CaptureStatus::Failed).problem(
                        "inputs_unavailable",
                        detail,
                        "check the session worktree, then resubmit",
                    ),
                ),
            },
        };
        Some(Self {
            policy,
            name,
            source,
            required: policy == CapturePolicy::Required,
            prepared,
        })
    }

    fn worktree(&self) -> Option<&Path> {
        self.prepared
            .as_ref()
            .ok()
            .map(|prepared| prepared.worktree.as_path())
    }

    /// Capture `head` against `base_ref`. `wait` chooses between waiting for
    /// another holder of the operation lock and reporting `in_progress`.
    fn run(
        &self,
        base_ref: &str,
        head: &str,
        wait: bool,
        open: &mut OpenStore<'_>,
    ) -> SubmitCaptureReport {
        let prepared = match &self.prepared {
            Ok(prepared) => prepared,
            Err(report) => return report.clone(),
        };
        let fresh = |status| SubmitCaptureReport::new(self.name, self.source, status);
        let request = match request(prepared, base_ref, head, self.policy) {
            Ok(request) => request,
            Err(detail) => {
                return fresh(CaptureStatus::Failed).problem(
                    "inputs_unavailable",
                    detail,
                    "check the session worktree, then resubmit",
                );
            }
        };
        let mut report = fresh(CaptureStatus::Failed);
        report.operation_id = Some(request.operation_id.as_str().to_string());
        report.base_commit = Some(request.base.as_str().to_string());
        report.result_commit = Some(request.result.as_str().to_string());
        let mut store = match open(&prepared.project) {
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
        let outcome = if wait {
            capture(&mut store, &request).map(Some)
        } else {
            try_capture(&mut store, &request)
        };
        match outcome {
            Ok(None) => {
                report.status = CaptureStatus::InProgress;
                report.problem(
                    "in_progress",
                    "another process is capturing this operation".into(),
                    "resubmit later to read its receipt",
                )
            }
            Ok(Some(CaptureOutcome::Acknowledged(receipt))) => {
                report.status = CaptureStatus::Acknowledged;
                report.receipt = Some(ReceiptReport::from(&receipt));
                report
            }
            Ok(Some(CaptureOutcome::Incomplete { code, detail, .. })) => {
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

/// Read the session's worktree and head.
fn inputs(
    broker: &mut crate::Broker,
    session: i64,
    project: ProjectKey,
) -> Result<Prepared, String> {
    let info = broker
        .store()
        .session(session)
        .map_err(|error| error.to_string())?;
    let worktree = PathBuf::from(&info.worktree_path);
    let checkout = crate::GitRepo::discover(&worktree).map_err(|error| error.to_string())?;
    let head = checkout.head_commit().map_err(|error| error.to_string())?;
    Ok(Prepared {
        project,
        session,
        worktree,
        head,
    })
}

/// The exact capture request for `result`, whose base is its merge base
/// with `base_ref`, the commit the submit verifies against.
fn request(
    prepared: &Prepared,
    base_ref: &str,
    result: &str,
    policy: CapturePolicy,
) -> Result<CaptureRequest, String> {
    let checkout =
        crate::GitRepo::discover(&prepared.worktree).map_err(|error| error.to_string())?;
    let base = checkout
        .merge_base(base_ref, result)
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
    use crate::collaboration_state::CollaborationRoot;
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
        _origin: Option<tempfile::TempDir>,
        _worktrees: tempfile::TempDir,
        state: tempfile::TempDir,
        broker: crate::Broker,
        session: i64,
        worktree: PathBuf,
    }

    /// A repository with `policy` capture and one committed session.
    fn fixture(policy: &str) -> Fixture {
        fixture_with(policy, false)
    }

    /// `capture = policy` in `.aethyme/config.toml`, committed on a fetched
    /// default branch (`origin/main`) when `committed`, otherwise only in the
    /// working copy.
    fn fixture_with(policy: &str, committed: bool) -> Fixture {
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
        let origin = committed.then(|| {
            let origin = tempfile::tempdir().unwrap();
            git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
            git(repo.path(), &["add", "-f", ".aethyme/config.toml"]);
            git(repo.path(), &["commit", "-qm", "config"]);
            git(
                repo.path(),
                &["remote", "add", "origin", origin.path().to_str().unwrap()],
            );
            git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
            git(repo.path(), &["remote", "set-head", "origin", "main"]);
            origin
        });
        let worktrees = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut broker = crate::Broker::open(repo.path())
            .unwrap()
            .with_worktree_root(worktrees.path())
            .with_collaboration_state(state.path());
        let session = broker.start_worktree("work", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        commit(&worktree, "first");
        Fixture {
            repo,
            _origin: origin,
            _worktrees: worktrees,
            state,
            broker,
            session: session.id,
            worktree,
        }
    }

    /// Integration is absent or still at `main`: nothing was promoted. Submit
    /// creates it from `main` when it does not exist yet.
    fn assert_integration_unmoved(repo: &Path) {
        let tip = git(
            repo,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/heads/aethyme/integration",
            ],
        );
        assert!(
            tip.is_empty() || tip == git(repo, &["rev-parse", "main"]),
            "integration moved to {tip}"
        );
    }

    fn commit(worktree: &Path, name: &str) -> String {
        std::fs::write(worktree.join(format!("{name}.txt")), "payload\n").unwrap();
        git(worktree, &["add", "-A"]);
        git(worktree, &["commit", "-qm", name]);
        git(worktree, &["rev-parse", "HEAD"])
    }

    fn opener(
        state: &Path,
    ) -> impl FnMut(&ProjectKey) -> Result<CollaborationStore, CollaborationStateError> + use<>
    {
        let state = state.to_path_buf();
        move |project| {
            CollaborationStore::open(&CollaborationRoot::under_host_state(&state), project, &[])
        }
    }

    /// Submit through [`submit_with`], running `between` before the submit
    /// pins its head; a pending advisory capture runs with the test store.
    fn submit_in_test(
        fixture: &mut Fixture,
        between: &mut dyn FnMut(),
    ) -> Result<(CaptureSubmit, Option<SubmitCaptureReport>), crate::BrokerOpError> {
        let mut open = opener(fixture.state.path());
        let result = submit_with(
            &mut fixture.broker,
            fixture.session,
            crate::CachePolicy::Use,
            crate::PromotionIntent::Configured,
            Some(&mut open),
            between,
        )?;
        let main_root = fixture.broker.main_root().to_path_buf();
        Ok(match result {
            CaptureSubmit::Submitted {
                outcome,
                capture: Some(CaptureStep::Pending(advisory)),
            } => {
                let report = advisory.run_with(&main_root, &mut open);
                (
                    CaptureSubmit::Submitted {
                        outcome,
                        capture: None,
                    },
                    Some(report),
                )
            }
            CaptureSubmit::Submitted {
                outcome,
                capture: Some(CaptureStep::Done(report)),
            } => (
                CaptureSubmit::Submitted {
                    outcome,
                    capture: None,
                },
                Some(report),
            ),
            other => (other, None),
        })
    }

    /// Required: the captured commit is the only one that may be submitted.
    /// A move after the capture is refused before any queue entry, the
    /// integration branch does not move, and the acknowledged report comes
    /// back with the refusal.
    #[test]
    fn a_session_moved_after_a_required_capture_is_not_submitted() {
        let mut fixture = fixture("required");

        let worktree = fixture.worktree.clone();
        let mut moved = String::new();
        let result = submit_in_test(&mut fixture, &mut || moved = commit(&worktree, "second"));
        match result {
            Ok((CaptureSubmit::HeadMoved { report, error }, _)) => {
                assert!(report.acknowledged());
                assert_ne!(report.result_commit.as_deref(), Some(moved.as_str()));
                assert!(matches!(
                    error,
                    crate::BrokerOpError::CapturedHeadMoved { ref actual, .. } if **actual == *moved
                ));
                assert_eq!(
                    crate::exit_status::for_broker_error(&error),
                    crate::exit_status::REFUSED
                );
            }
            Ok(_) => panic!("submitted a commit that was never captured"),
            Err(other) => panic!("unexpected error: {other}"),
        }
        assert!(fixture.broker.store().merge_queue().unwrap().is_empty());
        assert_integration_unmoved(fixture.repo.path());
    }

    /// Advisory: the receipt names the commit the submit actually pinned.
    #[test]
    fn an_advisory_capture_retains_the_commit_that_was_submitted() {
        let mut fixture = fixture("advisory");
        let worktree = fixture.worktree.clone();
        let mut moved = String::new();
        let (submitted, report) =
            submit_in_test(&mut fixture, &mut || moved = commit(&worktree, "second")).unwrap();
        let CaptureSubmit::Submitted { outcome, .. } = submitted else {
            panic!("advisory submit refused");
        };
        let report = report.unwrap();
        assert_eq!(outcome.entry.head_commit, moved);
        assert_eq!(report.status, CaptureStatus::Acknowledged, "{report:?}");
        assert_eq!(report.result_commit.as_deref(), Some(moved.as_str()));
    }

    /// Without a move, a required capture submits and promotes the captured
    /// commit; the promotion gate finds its receipt.
    #[test]
    fn a_required_capture_submits_the_captured_commit() {
        let mut fixture = fixture("required");
        let (submitted, report) = submit_in_test(&mut fixture, &mut || {}).unwrap();
        let CaptureSubmit::Submitted { outcome, .. } = submitted else {
            panic!("required submit failed");
        };
        let report = report.unwrap();
        assert_eq!(
            report.result_commit.as_deref(),
            Some(outcome.entry.head_commit.as_str())
        );
        assert!(outcome.promoted);
    }

    /// Under required, a submit that bypasses the capture (the library API,
    /// or any promotion path) verifies but never promotes an uncaptured head.
    #[test]
    fn an_uncaptured_head_is_never_promoted_under_required() {
        let mut fixture = fixture("required");
        let config = fixture.repo.path().join(".aethyme/config.toml");

        let error = fixture.broker.submit(fixture.session).unwrap_err();
        assert!(
            matches!(
                error,
                crate::BrokerOpError::CaptureRequiredForPromotion { .. }
            ),
            "{error}"
        );
        assert_eq!(
            crate::exit_status::for_broker_error(&error),
            crate::exit_status::REFUSED
        );
        assert_integration_unmoved(fixture.repo.path());
        let entry = fixture.broker.store().merge_queue().unwrap().pop().unwrap();
        assert_eq!(entry.status, crate::MergeStatus::Verified);
        assert!(matches!(
            fixture.broker.promote(entry.id),
            Err(crate::BrokerOpError::CaptureRequiredForPromotion { .. })
        ));
        // Advisory never gates promotion.
        std::fs::write(
            &config,
            "[collaboration]\ncapture = \"advisory\"\nproject = \"proj-test\"\n",
        )
        .unwrap();
        fixture.broker.promote(entry.id).unwrap();
    }

    /// A panicking capture is reported, not propagated.
    #[test]
    fn a_panicking_advisory_capture_is_a_failed_report() {
        let mut fixture = fixture("advisory");
        let mut open = opener(fixture.state.path());
        let CaptureSubmit::Submitted {
            capture: Some(CaptureStep::Pending(advisory)),
            ..
        } = submit_with(
            &mut fixture.broker,
            fixture.session,
            crate::CachePolicy::Use,
            crate::PromotionIntent::Configured,
            Some(&mut open),
            &mut || {},
        )
        .unwrap()
        else {
            panic!("expected a pending advisory capture");
        };
        let main_root = fixture.broker.main_root().to_path_buf();
        let report = advisory.run_with(&main_root, &mut |_| panic!("injected"));
        assert_eq!(report.status, CaptureStatus::Failed);
        assert_eq!(report.code.as_deref(), Some("panicked"));
    }

    /// An advisory capture whose operation another process is capturing
    /// reports `in_progress` instead of waiting.
    #[test]
    fn an_advisory_capture_does_not_wait_for_a_held_lock() {
        let mut fixture = fixture("advisory");
        let mut open = opener(fixture.state.path());
        let CaptureSubmit::Submitted {
            capture: Some(CaptureStep::Pending(advisory)),
            ..
        } = submit_with(
            &mut fixture.broker,
            fixture.session,
            crate::CachePolicy::Use,
            crate::PromotionIntent::Configured,
            Some(&mut open),
            &mut || {},
        )
        .unwrap()
        else {
            panic!("expected a pending advisory capture");
        };
        let prepared = advisory.capture.prepared.as_ref().ok().unwrap();
        let request = request(
            prepared,
            &advisory.base,
            &advisory.head,
            CapturePolicy::Advisory,
        )
        .unwrap();
        let store = open(&prepared.project).unwrap();
        let _held =
            crate::collaboration_capture::hold_operation_lock(&store, &request.operation_id);
        let main_root = fixture.broker.main_root().to_path_buf();
        let started = std::time::Instant::now();
        let report = advisory.run_with(&main_root, &mut open);
        assert_eq!(report.status, CaptureStatus::InProgress, "{report:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// What a schema-49 binary (every release before #660) decides about
    /// this repository's broker database.
    fn older_binary_opens(repo: &Path) -> bool {
        let conn = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
        let found = crate::schema::current_version(&conn).unwrap();
        crate::schema::schema_is_compatible_with(&conn, found, 49).unwrap()
    }

    fn fence(repo: &Path) -> Option<crate::CollaborationFence> {
        let conn = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
        crate::schema::collaboration_fence(&conn).unwrap()
    }

    /// Commit `policy` on the default branch from another clone and fetch
    /// it, leaving the main checkout's working copy as it was.
    fn commit_upstream_policy(fixture: &Fixture, policy: &str) {
        let origin = fixture._origin.as_ref().unwrap().path();
        let other = tempfile::tempdir().unwrap();
        git(
            other.path(),
            &["clone", "-q", origin.to_str().unwrap(), "."],
        );
        std::fs::write(
            other.path().join(".aethyme/config.toml"),
            format!("[collaboration]\ncapture = \"{policy}\"\nproject = \"proj-test\"\n"),
        )
        .unwrap();
        git(other.path(), &["commit", "-qam", policy]);
        git(other.path(), &["push", "-q", "origin", "HEAD:main"]);
        git(fixture.repo.path(), &["fetch", "-q", "origin"]);
    }

    fn effective_fence(broker: &crate::Broker) -> Option<crate::CollaborationFence> {
        let config = crate::merge::repository_config_with_source(broker.main_root());
        broker.collaboration_fence_state(crate::broker::FenceTrigger::Config(config.as_ref()))
    }

    /// Any writable open by a #660 binary of a repository whose committed
    /// config requires capture fences older binaries out; off and advisory
    /// leave the floor alone.
    #[test]
    fn opening_a_required_repository_fences_older_binaries_out() {
        let required = fixture_with("required", true);
        assert_eq!(
            fence(required.repo.path()),
            Some(crate::CollaborationFence {
                min_compatible_schema: crate::COLLABORATION_FENCE_SCHEMA,
                reason: crate::schema::COLLABORATION_FENCE_REASON.into(),
                source: "committed".into(),
                state: "active",
            })
        );
        assert!(!older_binary_opens(required.repo.path()));
        for policy in ["advisory", "off"] {
            let fixture = fixture_with(policy, true);
            assert_eq!(fence(fixture.repo.path()), None, "{policy}");
            assert!(older_binary_opens(fixture.repo.path()), "{policy}");
        }
    }

    /// Only the committed copy fences: an uncommitted or experimental
    /// `required` in the main checkout never locks older binaries out.
    #[test]
    fn an_uncommitted_required_does_not_fence() {
        // No fetched default branch at all: the working copy alone.
        let local = fixture_with("required", false);
        assert_eq!(fence(local.repo.path()), None);
        assert!(effective_fence(&local.broker).is_none());
        assert!(older_binary_opens(local.repo.path()));

        // Committed off, working copy edited to required.
        let fixture = fixture_with("off", true);
        std::fs::write(
            fixture.repo.path().join(".aethyme/config.toml"),
            "[collaboration]\ncapture = \"required\"\nproject = \"proj-test\"\n",
        )
        .unwrap();
        let reopened = crate::Broker::open(fixture.repo.path()).unwrap();
        assert!(effective_fence(&reopened).is_none());
        assert_eq!(fence(fixture.repo.path()), None);
        assert!(older_binary_opens(fixture.repo.path()));
    }

    /// The fence is one-way: committing required off does not let older
    /// binaries back in.
    #[test]
    fn turning_required_off_keeps_the_fence() {
        let fixture = fixture_with("required", true);
        commit_upstream_policy(&fixture, "off");
        std::fs::write(
            fixture.repo.path().join(".aethyme/config.toml"),
            "[collaboration]\ncapture = \"off\"\n",
        )
        .unwrap();
        let reopened = crate::Broker::open(fixture.repo.path()).unwrap();
        assert_eq!(effective_fence(&reopened).unwrap().state, "active");
        assert!(!older_binary_opens(fixture.repo.path()));
    }

    /// A typo is refused at submit but is not a reason to lock older binaries
    /// out for good: only an explicit required raises the floor.
    #[test]
    fn an_unsupported_value_does_not_raise_the_floor() {
        let fixture = fixture_with("requried", true);
        assert!(effective_fence(&fixture.broker).is_none());
        assert_eq!(fence(fixture.repo.path()), None);
        assert!(older_binary_opens(fixture.repo.path()));
    }

    /// Read-only status (snapshot opens: `status --read-only-snapshot`,
    /// graph refresh preconditions) never writes the fence and never fails
    /// because of it: with `required` committed upstream while the main
    /// checkout's working copy trails, it reports the fence as pending. A
    /// writable status then raises it.
    #[test]
    fn read_only_status_reports_a_pending_fence_and_writes_nothing() {
        let fixture = fixture_with("off", true);
        commit_upstream_policy(&fixture, "required");
        let snapshot = crate::Broker::open_snapshot(fixture.repo.path()).unwrap();
        for view in [
            snapshot.status_snapshot(0).unwrap(),
            snapshot.status_current_snapshot(0).unwrap(),
        ] {
            assert_eq!(view.collaboration_fence.as_ref().unwrap().state, "pending");
            assert!(
                view.advice
                    .iter()
                    .any(|advice| advice.id == "collaboration.fence-pending"),
                "{:?}",
                view.advice
            );
        }
        assert_eq!(fence(fixture.repo.path()), None);
        assert!(older_binary_opens(fixture.repo.path()));

        let mut writable = crate::Broker::open(fixture.repo.path()).unwrap();
        let view = writable.status_current(0).unwrap();
        assert_eq!(view.collaboration_fence.unwrap().state, "active");
        assert!(!older_binary_opens(fixture.repo.path()));
    }

    /// Without any broker database, the snapshot status runs on an in-memory
    /// store: it still reports the pending fence and creates nothing.
    #[test]
    fn a_snapshot_without_a_database_reports_a_pending_fence() {
        let repo = tempfile::tempdir().unwrap();
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(
            repo.path().join(".aethyme/config.toml"),
            "[collaboration]\ncapture = \"required\"\nproject = \"proj-test\"\n",
        )
        .unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["add", "-f", ".aethyme/config.toml"]);
        git(repo.path(), &["commit", "-qm", "init"]);
        git(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
        git(repo.path(), &["remote", "set-head", "origin", "main"]);
        let snapshot = crate::Broker::open_snapshot(repo.path()).unwrap();
        let view = snapshot.status_snapshot(0).unwrap();
        assert_eq!(view.collaboration_fence.unwrap().state, "pending");
        assert!(!repo.path().join(".aethyme/broker.db").exists());
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
                matches!(
                    setting_from_text(Some(&text)),
                    Setting::Unsupported {
                        code: "unsupported_policy",
                        ..
                    }
                ),
                "{value}"
            );
        }
    }

    /// Every [collaboration] setting is critical once capture is on: one
    /// this binary does not implement is refused, never ignored. Without
    /// capture enabled the repository keeps the legacy submit (#680).
    #[test]
    fn an_unknown_setting_is_refused_once_capture_is_on() {
        for policy in ["advisory", "required"] {
            let text = format!(
                "[collaboration]\ncapture = \"{policy}\"\nproject = \"p\"\nbrief_policy = \"x\"\n"
            );
            assert!(
                matches!(
                    setting_from_text(Some(&text)),
                    Setting::Unsupported {
                        code: "unknown_setting",
                        ..
                    }
                ),
                "{policy}"
            );
        }
        for text in [
            "[collaboration]\nbrief_policy = \"x\"\n",
            "[collaboration]\ncapture = \"off\"\nbrief_policy = \"x\"\n",
        ] {
            assert_eq!(setting_from_text(Some(text)), Setting::Off, "{text}");
        }
    }

    /// A malformed file fails closed only when it visibly opted in; a
    /// repository that never mentions collaboration keeps the legacy submit.
    #[test]
    fn a_malformed_opt_in_fails_closed_and_other_malformed_files_stay_off() {
        for text in [
            "[collaboration]\ncapture = \"required\"\nproject = \n",
            "[ collaboration ]\ncapture = required\n",
            "collaboration.capture = \"required\"\n[promote\n",
            "collaboration = { capture = \"required\" \n",
        ] {
            assert!(
                matches!(
                    setting_from_text(Some(text)),
                    Setting::Unsupported {
                        code: "config_unreadable",
                        ..
                    }
                ),
                "{text}"
            );
        }
        for text in ["collaboration = \"required\"\n", "collaboration = true\n"] {
            assert!(
                matches!(
                    setting_from_text(Some(text)),
                    Setting::Unsupported {
                        code: "unsupported_policy",
                        ..
                    }
                ),
                "{text}"
            );
        }
        for text in ["[promote\nmode = auto\n", "# collaboration later\n[gates"] {
            assert_eq!(setting_from_text(Some(text)), Setting::Off, "{text}");
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

    #[test]
    fn details_carry_no_host_paths() {
        let known = vec![
            (
                PathBuf::from("/Users/someone/Library/Application Support/Aethyme"),
                "<host state>",
            ),
            (PathBuf::from("/Users/someone"), "<home>"),
        ];
        let detail = "collaboration root /Users/someone/Library/Application Support/Aethyme/collaboration \
                      is inside the worktree container \"/Volumes/T7 drive/Application Support/x\"; \
                      move /opt/thing elsewhere";
        let clean = path_free(detail, &known);
        assert!(!clean.contains("/Users"), "{clean}");
        assert!(!clean.contains("/Volumes"), "{clean}");
        assert!(!clean.contains("/opt"), "{clean}");
        assert!(clean.contains("<host state>/collaboration"), "{clean}");
        assert!(clean.contains("move <path> elsewhere"), "{clean}");
    }
}
