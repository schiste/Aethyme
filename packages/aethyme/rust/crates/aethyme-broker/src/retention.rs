//! Repository-declared broker retention policy.
//!
//! Missing configuration is intentional: conservative shipped defaults keep
//! normal repositories bounded without making enrollment depend on another
//! generated file. Maintainers may override the policy in
//! `.aethyme/broker.toml`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const BROKER_CONFIG_RELPATH: &str = ".aethyme/broker.toml";
pub const RETENTION_POLICY_SCHEMA_VERSION: u32 = 1;

const RETENTION_POLICY_FIELDS: &[&str] = &[
    "schema_version",
    "terminal_events_days",
    "gate_results_days",
    "terminal_merge_queue_days",
    "command_metrics_days",
    "closed_worktrees_days",
    "publication_exposure_days",
    "retained_bytes_budget",
    "artifact_reclaim_days",
    "orphan_worktree_roots_days",
    "session_abandoned_after_hours",
    "artifact_sweep_budget_ms",
    "artifact_sweep_interval_hours",
    "artefact_directories",
    "startup_budget_ms",
    "routine_size_budget_ms",
    "size_record_ttl_hours",
];

/// Directory names that are repository source or control roots rather than
/// regenerable build output. Configured artefact names use a deliberately
/// broad non-empty-directory witness, so allowing these names would let a
/// typo turn normal repository content into a deletion candidate.
///
/// Two groups, and the second carries the larger loss. Source and tooling roots
/// (`src`, `lib`, `tests`, `docs`) are tracked, so removing one costs a
/// checkout. Data roots (`data`, `fixtures`, `logs`, `coverage`) are usually
/// ignored, and being ignored is exactly what makes them grow large enough to
/// tempt an operator into listing them -- and unrecoverable once removed. That
/// is the case the built-in catalog's fixed list was written to refuse:
/// "large and ignored" also matches a downloaded dataset or a local database
/// someone cannot rebuild.
const PROTECTED_ARTEFACT_DIRECTORY_NAMES: &[&str] = &[
    ".aethyme", ".git", ".github", ".gitlab", ".idea", ".vscode", "coverage", "data", "doc",
    "docs", "example", "examples", "fixtures", "include", "lib", "logs", "src", "test", "tests",
];

pub(crate) fn is_safe_artefact_directory_name(name: &str) -> bool {
    !PROTECTED_ARTEFACT_DIRECTORY_NAMES
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionPolicy {
    pub schema_version: u32,
    pub terminal_events_days: u32,
    pub gate_results_days: u32,
    pub terminal_merge_queue_days: u32,
    pub command_metrics_days: u32,
    /// Days an unverified promoted publication may remain outstanding before
    /// it becomes an explicit terminal expiry rather than an eternal blocker.
    pub publication_exposure_days: u32,
    /// Soft repository storage budget. `0` disables budget warnings.
    pub retained_bytes_budget: u64,
    /// Idle days before a closed session's unproven worktree is eligible for
    /// GC. Worktrees with representation proof are eligible regardless of age;
    /// build caches use `artifact_reclaim_days` instead. This does not affect
    /// committed work or authorize removal without provenance proof.
    pub closed_worktrees_days: u32,
    /// Idle days before a closed session's build caches are reclaimed without
    /// confirmation. This does not affect committed work or the worktree
    /// itself; a maintainer may raise it to trade disk space for faster reuse.
    pub artifact_reclaim_days: u32,
    pub orphan_worktree_roots_days: u32,
    /// Hours a session may go without any evidence of a working agent
    /// before the broker concludes the agent is gone and stops letting
    /// the session pin its worktree and branch. `0` disables the lane and
    /// restores the previous unbounded hold.
    ///
    /// This only makes a worktree a cleanup *candidate*. Dirty trees,
    /// unpromoted commits, and unproven provenance still block removal, so
    /// raising this trades disk against how long a dead session's disk is
    /// held -- never against whether committed work survives.
    pub session_abandoned_after_hours: u32,
    /// Wall-clock budget for the autonomous artifact sweep. `0` disables it.
    pub artifact_sweep_budget_ms: u64,
    /// Minimum spacing between autonomous artifact sweeps.
    pub artifact_sweep_interval_hours: u32,
    /// Additional single-component directory names that may be treated as
    /// regenerable artifacts. This is additive to the built-in catalog; an
    /// operator cannot use configuration to weaken a built-in witness.
    pub artefact_directories: Vec<String>,
    pub startup_budget_ms: u64,
    /// Wall-clock budget a *routine* check -- `broker status`, `doctor` --
    /// may spend measuring one directory it has never sized. `0` disables
    /// warming -- no walk clears a deadline that has already passed -- leaving
    /// routine totals frozen at whatever `gc plan` last recorded.
    ///
    /// This is not a budget for sizing everything. Routine checks read
    /// recorded sizes and walk nothing; this buys exactly one measurement per
    /// pass so a machine nobody audits still converges on knowing its own
    /// size. The ceiling is deliberately small: a routine check that can
    /// afford the whole walk has become the expensive walk again (#176).
    pub routine_size_budget_ms: u64,
    /// How long a recorded directory size is treated as current. Past this, a
    /// routine check prefers to spend its measurement budget refreshing it.
    pub size_record_ttl_hours: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            schema_version: RETENTION_POLICY_SCHEMA_VERSION,
            terminal_events_days: 180,
            gate_results_days: 30,
            terminal_merge_queue_days: 180,
            command_metrics_days: 30,
            closed_worktrees_days: 7,
            publication_exposure_days: 30,
            retained_bytes_budget: 1_073_741_824,
            artifact_reclaim_days: 0,
            orphan_worktree_roots_days: 1,
            session_abandoned_after_hours: 72,
            artifact_sweep_budget_ms: 5_000,
            artifact_sweep_interval_hours: 24,
            artefact_directories: Vec::new(),
            startup_budget_ms: 25,
            routine_size_budget_ms: 200,
            size_record_ttl_hours: 24,
        }
    }
}

impl RetentionPolicy {
    pub fn validate(&self) -> Result<(), RetentionConfigError> {
        if self.schema_version != RETENTION_POLICY_SCHEMA_VERSION {
            return Err(RetentionConfigError::UnsupportedSchema {
                found: self.schema_version,
                supported: RETENTION_POLICY_SCHEMA_VERSION,
            });
        }
        for (field, value) in [
            ("terminal_events_days", self.terminal_events_days),
            ("gate_results_days", self.gate_results_days),
            ("terminal_merge_queue_days", self.terminal_merge_queue_days),
            ("command_metrics_days", self.command_metrics_days),
            ("closed_worktrees_days", self.closed_worktrees_days),
            ("publication_exposure_days", self.publication_exposure_days),
        ] {
            if value == 0 || value > 36_500 {
                return Err(RetentionConfigError::InvalidValue {
                    field,
                    value: value.to_string(),
                    constraint: "must be between 1 and 36500 days",
                });
            }
        }
        if self.retained_bytes_budget > 1_125_899_906_842_624 {
            return Err(RetentionConfigError::InvalidValue {
                field: "retained_bytes_budget",
                value: self.retained_bytes_budget.to_string(),
                constraint: "must be between 0 (warnings disabled) and 1 PiB",
            });
        }
        // These two accept 0, meaning no grace period. Neither removes
        // committed work, so waiting is a convenience rather than a safeguard.
        for (field, value) in [
            ("artifact_reclaim_days", self.artifact_reclaim_days),
            (
                "orphan_worktree_roots_days",
                self.orphan_worktree_roots_days,
            ),
        ] {
            if value > 36_500 {
                return Err(RetentionConfigError::InvalidValue {
                    field,
                    value: value.to_string(),
                    constraint: "must be between 0 (no grace period) and 36500 days",
                });
            }
        }
        // 0 disables the lane. The ceiling is a year: a longer window is
        // indistinguishable from the unbounded hold this field exists to
        // end, and is almost certainly a units mistake.
        if self.session_abandoned_after_hours > 8_760 {
            return Err(RetentionConfigError::InvalidValue {
                field: "session_abandoned_after_hours",
                value: self.session_abandoned_after_hours.to_string(),
                constraint: "must be between 0 (disabled) and 8760 hours",
            });
        }
        if self.artifact_sweep_budget_ms > 60_000 {
            return Err(RetentionConfigError::InvalidValue {
                field: "artifact_sweep_budget_ms",
                value: self.artifact_sweep_budget_ms.to_string(),
                constraint: "must be between 0 (disabled) and 60000 milliseconds",
            });
        }
        if !(1..=8_760).contains(&self.artifact_sweep_interval_hours) {
            return Err(RetentionConfigError::InvalidValue {
                field: "artifact_sweep_interval_hours",
                value: self.artifact_sweep_interval_hours.to_string(),
                constraint: "must be between 1 and 8760 hours",
            });
        }
        for directory in &self.artefact_directories {
            let mut components = Path::new(directory).components();
            let valid = !directory.is_empty()
                && !directory.contains('/')
                && !directory.contains('\\')
                && !directory.contains('\0')
                && matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none();
            if !valid || !is_safe_artefact_directory_name(directory) {
                return Err(RetentionConfigError::InvalidValue {
                    field: "artefact_directories",
                    value: directory.clone(),
                    constraint: "each entry must be one non-empty safe directory name without path separators or reserved source/control names",
                });
            }
        }
        // A routine check may spend at most a quarter second measuring. Any
        // larger and the split this bounds -- routine check against expensive
        // walk -- stops being a split.
        if self.routine_size_budget_ms > 250 {
            return Err(RetentionConfigError::InvalidValue {
                field: "routine_size_budget_ms",
                value: self.routine_size_budget_ms.to_string(),
                constraint: "must be between 0 (no warming) and 250 milliseconds",
            });
        }
        if !(1..=8_760).contains(&self.size_record_ttl_hours) {
            return Err(RetentionConfigError::InvalidValue {
                field: "size_record_ttl_hours",
                value: self.size_record_ttl_hours.to_string(),
                constraint: "must be between 1 and 8760 hours",
            });
        }
        if !(1..=5_000).contains(&self.startup_budget_ms) {
            return Err(RetentionConfigError::InvalidValue {
                field: "startup_budget_ms",
                value: self.startup_budget_ms.to_string(),
                constraint: "must be between 1 and 5000 milliseconds",
            });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RetentionConfigError {
    #[error("cannot read broker retention config at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("broker.toml: {0}")]
    Parse(String),
    #[error("unsupported retention policy schema {found}; this binary supports schema {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("retention.{field}={value} is invalid: {constraint}")]
    InvalidValue {
        field: &'static str,
        value: String,
        constraint: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct RetentionConfigWarning {
    /// The dotted configuration key that was ignored.
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for RetentionConfigWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RetentionPolicyLoadReport {
    pub policy: RetentionPolicy,
    pub warnings: Vec<RetentionConfigWarning>,
}

fn unknown_retention_field_warning(field: String, schema_version: u32) -> RetentionConfigWarning {
    RetentionConfigWarning {
        message: format!(
            "unknown field `{field}` for retention schema {schema_version}; ignored so known retention settings remain active; check its spelling or upgrade Aethyme if intentional"
        ),
        field,
    }
}

fn unknown_broker_field_warning(field: String) -> RetentionConfigWarning {
    RetentionConfigWarning {
        message: format!(
            "unknown broker configuration field `{field}`; ignored so known broker settings remain active; check its spelling or upgrade Aethyme if intentional"
        ),
        field,
    }
}

/// Read the version marker before deserializing the policy's field set.
///
/// The version is the compatibility boundary: an older binary can safely
/// ignore a field it does not know while still applying the fields it does
/// know, but it must refuse a schema whose semantics it cannot interpret.
pub fn load_retention_policy_report(
    repo: &Path,
) -> Result<RetentionPolicyLoadReport, RetentionConfigError> {
    let path = repo.join(BROKER_CONFIG_RELPATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RetentionPolicyLoadReport {
                policy: RetentionPolicy::default(),
                warnings: Vec::new(),
            });
        }
        Err(source) => return Err(RetentionConfigError::Io { path, source }),
    };

    let root: toml::Value = toml::from_str(&text)
        .map_err(|error: toml::de::Error| RetentionConfigError::Parse(error.to_string()))?;
    let root = root.as_table().ok_or_else(|| {
        RetentionConfigError::Parse("the document root must be a TOML table".into())
    })?;
    let mut warnings = Vec::new();
    for field in root.keys().filter(|field| field.as_str() != "retention") {
        warnings.push(unknown_broker_field_warning(format!("broker.{field}")));
    }

    let retention = match root.get("retention") {
        None => toml::map::Map::new(),
        Some(toml::Value::Table(retention)) => retention.clone(),
        Some(_) => {
            return Err(RetentionConfigError::Parse(
                "the `retention` value must be a TOML table".into(),
            ));
        }
    };
    let schema_version = match retention.get("schema_version") {
        None => RETENTION_POLICY_SCHEMA_VERSION,
        Some(toml::Value::Integer(value)) if (0..=i64::from(u32::MAX)).contains(value) => {
            *value as u32
        }
        Some(_) => {
            return Err(RetentionConfigError::Parse(
                "retention.schema_version must be an unsigned integer".into(),
            ));
        }
    };
    if schema_version != RETENTION_POLICY_SCHEMA_VERSION {
        return Err(RetentionConfigError::UnsupportedSchema {
            found: schema_version,
            supported: RETENTION_POLICY_SCHEMA_VERSION,
        });
    }

    let mut known_fields = toml::map::Map::new();
    for (field, value) in retention {
        if RETENTION_POLICY_FIELDS.contains(&field.as_str()) {
            known_fields.insert(field, value);
        } else {
            warnings.push(unknown_retention_field_warning(
                format!("retention.{field}"),
                schema_version,
            ));
        }
    }
    warnings.sort_by(|left, right| left.field.cmp(&right.field));

    let policy: RetentionPolicy = toml::Value::Table(known_fields)
        .try_into()
        .map_err(|error: toml::de::Error| RetentionConfigError::Parse(error.to_string()))?;
    policy.validate()?;
    Ok(RetentionPolicyLoadReport { policy, warnings })
}

pub fn load_retention_policy(repo: &Path) -> Result<RetentionPolicy, RetentionConfigError> {
    Ok(load_retention_policy_report(repo)?.policy)
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GcRowKind {
    Event,
    GateResult,
    Advisory,
    EntryExposure,
    IntegrationReconciliationEntry,
    MergeQueue,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcRowCandidate {
    pub kind: GcRowKind,
    pub id: i64,
    pub recorded_at: i64,
    pub estimated_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_log_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GcFileAction {
    Delete,
    Rewrite,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcFileCandidate {
    pub path: String,
    pub action: GcFileAction,
    pub before_sha256: String,
    pub after_sha256: Option<String>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub source_row_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcWorktreeCandidate {
    pub session_id: i64,
    pub worktree_path: String,
    pub worktree_present: bool,
    pub branch_ref: String,
    pub branch_tip: Option<String>,
    pub estimated_bytes: u64,
    pub closed_at: i64,
}

/// A git-ignored build directory inside a retained worktree.
///
/// Reclaiming these is provenance-neutral: they hold no committed work, so
/// they are recoverable by rebuilding and are considered independently of the
/// worktree's own cleanup disposition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcArtifactCandidate {
    pub session_id: i64,
    pub worktree_path: String,
    /// Path of the build directory relative to the worktree root.
    pub relative_dir: String,
    pub estimated_bytes: u64,
    pub idle_days: u32,
}

/// A large git-ignored directory that was deliberately not classified as
/// regenerable artifact output. This is evidence for an operator, never a GC
/// candidate: the broker does not infer that an arbitrary ignored directory is
/// safe to delete.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcDeclinedArtifact {
    pub session_id: i64,
    pub worktree_path: String,
    /// Path of the ignored directory relative to the worktree root.
    pub relative_dir: String,
    pub estimated_bytes: u64,
    pub reason: String,
}

/// A host worktree root whose owning repository no longer exists.
///
/// Worktree storage is host-scoped but ownership records are repository-local,
/// so a deleted repository leaves its worktree tree with no database that can
/// ever account for it. The `.aethyme-worktree-root.json` breadcrumb is the
/// reverse pointer that makes these recoverable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcOrphanCandidate {
    pub repository_key: String,
    pub worktree_root: String,
    pub repository_root: String,
    pub estimated_bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcBlocker {
    pub kind: String,
    pub id: Option<i64>,
    pub reason: String,
}

/// A closed session's accepted queue checkpoint that still has a GC pin.
///
/// Releasing this record changes only broker metadata. The accepted session
/// head, integration commit, and tree remain available for cleanup provenance,
/// and no committed worktree or branch is touched.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcCheckpointPinRelease {
    pub session_id: i64,
    pub queue_entry_id: i64,
    pub recorded_at: i64,
    pub estimated_bytes: u64,
    pub reason: String,
}

/// An outstanding publication exposure past its stated retention age. The
/// plan names the exact row; only an explicit `gc apply` may expire it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcPublicationExposureExpiry {
    pub exposure_id: i64,
    pub queue_entry_id: i64,
    pub created_at: i64,
    pub age_days: u32,
    pub reason: String,
}

/// An aggregate view of the protections in a GC plan.
///
/// The individual blocker list remains the authoritative explanation for each
/// row. This companion view makes retained disk pressure and ageing actionable
/// by grouping it by the rule that held it, with the largest group first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcBlockerSummary {
    pub kind: String,
    pub count: usize,
    pub retained_bytes: u64,
    /// The row or session identifier of the oldest member, when the blocker
    /// kind has one. A null value means the kind only has path-level or
    /// otherwise unaddressable findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_recorded_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_age_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_policy_days: Option<u32>,
    #[serde(default)]
    pub age_exceeded: bool,
}

/// A byte-backed aggregation of the worktree blockers in a GC plan.
///
/// The individual blocker list remains the authoritative explanation for each
/// session. This companion view makes the retained disk pressure actionable by
/// grouping it by the rule that held it, with the largest group first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcWorktreeBlockerSummary {
    pub kind: String,
    pub count: usize,
    pub retained_bytes: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GcPlan {
    pub schema_version: u32,
    pub digest: String,
    pub evaluated_at: i64,
    pub policy: RetentionPolicy,
    /// Unknown fields are ignored for forward compatibility, but remain in
    /// the plan so the operator sees exactly what this binary did not apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retention_config_warnings: Vec<RetentionConfigWarning>,
    pub rows: Vec<GcRowCandidate>,
    pub files: Vec<GcFileCandidate>,
    pub worktrees: Vec<GcWorktreeCandidate>,
    pub artifacts: Vec<GcArtifactCandidate>,
    pub orphans: Vec<GcOrphanCandidate>,
    pub blockers: Vec<GcBlocker>,
    /// Closed-session checkpoint pins that a reviewed `gc apply` may release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoint_pin_releases: Vec<GcCheckpointPinRelease>,
    /// Publication exposures past policy age that a reviewed `gc apply` may
    /// move to the terminal `expired` state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_exposure_expiries: Vec<GcPublicationExposureExpiry>,
    /// Protection counts and byte estimates grouped by blocker kind. This is
    /// reporting only and intentionally excluded from the authorization
    /// digest, like the other measured byte totals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_summary: Vec<GcBlockerSummary>,
    /// Retained worktree bytes grouped by the blocker that holds them. This
    /// is reporting only and intentionally excluded from the authorization
    /// digest, like the other measured byte totals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_blocker_summary: Vec<GcWorktreeBlockerSummary>,
    /// Large ignored directories outside the safe artifact catalog. These
    /// are reporting-only and excluded from the authorization digest.
    #[serde(default)]
    pub declined_artifacts: Vec<GcDeclinedArtifact>,
    pub estimated_reclaimable_bytes: u64,
    /// Every byte held by retained worktrees, whether or not this plan acts on
    /// it. Reporting only: excluded from the digest so measured sizes never
    /// invalidate an authorization.
    pub estimated_retained_bytes: u64,
    /// Bytes this plan deliberately leaves in place because a retention or
    /// provenance gate blocked them.
    pub estimated_blocked_bytes: u64,
    /// Bytes in [`GcPlan::declined_artifacts`]. Kept separate from
    /// `estimated_reclaimable_bytes` so evidence can never be mistaken for a
    /// deletion authorization.
    #[serde(default)]
    pub estimated_declined_artifact_bytes: u64,
    /// Directories under a broker worktree root that no session row claims
    /// (#176). Reporting only, and excluded from the digest for the same
    /// reason the byte totals are: this plan does not act on these, so a
    /// directory appearing or vanishing must not invalidate an operator's
    /// authorization to remove something else.
    ///
    /// Which rule the candidate lists above are ordered by, and therefore
    /// what a time-bounded `gc apply` will reach before its deadline.
    #[serde(default = "default_reclaim_order")]
    pub reclaim_order: crate::ReclaimOrder,
    /// Bytes above `retained_bytes_budget`. `0` within budget or unset.
    /// Reporting only: excluded from the digest with the other totals.
    #[serde(default)]
    pub retained_bytes_deficit: u64,
    /// Whether applying all of this plan would bring retention under budget.
    /// `false` with a non-zero deficit is a budget no reclamation can satisfy,
    /// which is a different problem from a backlog and reads differently.
    #[serde(default = "default_clears_budget")]
    pub clears_retained_bytes_budget: bool,
    /// What the byte totals are actually able to conclude about the budget.
    /// A plan assembled without walking the trees reports floors, and a floor
    /// answers "over budget" but never "within budget" (#176).
    #[serde(default = "default_budget_verdict")]
    pub budget_verdict: crate::BudgetVerdict,
    /// Directories whose bytes are missing from the totals above because
    /// nobody walked them -- retained worktrees and orphaned worktree roots
    /// alike. `0` on the full audit, which measures everything it lists.
    #[serde(default)]
    pub unmeasured_directory_count: usize,
    /// The oldest recorded measurement behind the byte totals.
    #[serde(default)]
    pub sizes_measured_at_ms: Option<i64>,
    /// `None` only when re-reading a plan written before the sweep existed.
    /// That is deliberately not the same value as a sweep that found nothing:
    /// a plan that never looked must not read as a plan that looked and found
    /// a clean root.
    #[serde(default)]
    pub reconciliation: Option<crate::WorktreeReconciliation>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GcApplyReport {
    pub digest: String,
    pub complete: bool,
    pub deadline_reached: bool,
    pub rows_removed: usize,
    pub files_completed: Vec<String>,
    pub sessions_cleaned: Vec<i64>,
    pub artifacts_reclaimed: Vec<String>,
    pub orphans_removed: Vec<String>,
    pub checkpoint_pins_released: Vec<i64>,
    pub publication_exposures_expired: Vec<i64>,
    pub reclaimed_bytes: u64,
    pub failures: Vec<String>,
    pub recovery_action: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GcHealth {
    pub policy: RetentionPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retention_config_warnings: Vec<RetentionConfigWarning>,
    pub pending_recovery_digest: Option<String>,
    pub candidate_rows: usize,
    pub candidate_files: usize,
    pub candidate_worktrees: usize,
    pub candidate_artifacts: usize,
    pub candidate_orphans: usize,
    pub estimated_reclaimable_bytes: u64,
    pub estimated_retained_bytes: u64,
    pub estimated_blocked_bytes: u64,
    pub over_retained_bytes_budget: bool,
    pub retained_bytes_deficit: u64,
    pub clears_retained_bytes_budget: bool,
    pub reclaim_order: crate::ReclaimOrder,
    /// What the byte totals can conclude about the budget. `doctor` reads
    /// recorded sizes, so this is where an unmeasured remainder surfaces.
    pub budget_verdict: crate::BudgetVerdict,
    /// Directories whose bytes are missing from the totals above. Counts
    /// orphaned worktree roots as well as retained worktrees, because an
    /// unsized orphan is exactly as absent from the total.
    pub unmeasured_directory_count: usize,
    pub sizes_measured_at_ms: Option<i64>,
    pub blockers: usize,
    /// Directories under a broker worktree root that no session claims.
    pub unclaimed_worktree_count: usize,
    pub unclaimed_worktree_bytes: u64,
}

/// A plan written before ordering was policy-driven was ordered by session id,
/// which is what oldest-first approximates.
fn default_reclaim_order() -> crate::ReclaimOrder {
    crate::ReclaimOrder::OldestFirst
}

/// An older plan carried no budget verdict. Reading it as "cannot be cleared"
/// would raise an alarm about a plan nobody can re-evaluate.
fn default_clears_budget() -> bool {
    true
}

/// Every plan written before the cheap path existed came from the full walk,
/// so its totals were measurements. `Unknown` would claim those plans skipped
/// something; they did not.
fn default_budget_verdict() -> crate::BudgetVerdict {
    crate::BudgetVerdict::Within
}

impl GcPlan {
    pub fn finish_digest(&mut self) -> Result<(), serde_json::Error> {
        #[derive(serde::Serialize)]
        struct Authorization<'a> {
            schema_version: u32,
            policy: &'a RetentionPolicy,
            rows: &'a [GcRowCandidate],
            files: &'a [GcFileCandidate],
            worktrees: &'a [GcWorktreeCandidate],
            artifacts: &'a [GcArtifactCandidate],
            orphans: &'a [GcOrphanCandidate],
            blockers: &'a [GcBlocker],
            checkpoint_pin_releases: &'a [GcCheckpointPinRelease],
            publication_exposure_expiries: &'a [GcPublicationExposureExpiry],
        }
        let bytes = serde_json::to_vec(&Authorization {
            schema_version: self.schema_version,
            policy: &self.policy,
            rows: &self.rows,
            files: &self.files,
            worktrees: &self.worktrees,
            artifacts: &self.artifacts,
            orphans: &self.orphans,
            blockers: &self.blockers,
            checkpoint_pin_releases: &self.checkpoint_pin_releases,
            publication_exposure_expiries: &self.publication_exposure_expiries,
        })?;
        self.digest = format!("{:x}", Sha256::digest(bytes));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn missing_config_uses_conservative_bounded_defaults() {
        let repo = tempfile::tempdir().unwrap();
        let policy = load_retention_policy(repo.path()).unwrap();
        assert_eq!(policy, RetentionPolicy::default());
        assert!(policy.terminal_events_days >= policy.gate_results_days);
        assert!(policy.terminal_merge_queue_days >= policy.gate_results_days);
        assert!(policy.startup_budget_ms <= 25);
        assert_eq!(policy.artifact_reclaim_days, 0);
        assert_eq!(policy.artifact_sweep_budget_ms, 5_000);
        assert_eq!(policy.routine_size_budget_ms, 200);
        assert_eq!(policy.size_record_ttl_hours, 24);
        assert_eq!(policy.retained_bytes_budget, 1_073_741_824);
        assert!(policy.artefact_directories.is_empty());
    }

    #[test]
    fn partial_policy_preserves_new_field_defaults_across_upgrades() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(BROKER_CONFIG_RELPATH),
            "[retention]\ngate_results_days = 14\n",
        )
        .unwrap();

        let policy = load_retention_policy(repo.path()).unwrap();
        assert_eq!(policy.gate_results_days, 14);
        assert_eq!(
            policy.terminal_events_days,
            RetentionPolicy::default().terminal_events_days
        );
        assert_eq!(
            policy.startup_budget_ms,
            RetentionPolicy::default().startup_budget_ms
        );
    }

    #[test]
    fn configured_artifact_directories_are_additive_and_path_scoped() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(BROKER_CONFIG_RELPATH),
            "[retention]\nartefact_directories = [\".pnpm-store\", \"cache\"]\n",
        )
        .unwrap();

        let policy = load_retention_policy(repo.path()).unwrap();
        assert_eq!(
            policy.artefact_directories,
            vec![".pnpm-store".to_string(), "cache".to_string()]
        );

        for directory in ["", ".", "..", "nested/cache", "/tmp", "foo\\bar"] {
            let mut policy = RetentionPolicy::default();
            policy.artefact_directories = vec![directory.into()];
            assert!(
                matches!(
                    policy.validate(),
                    Err(RetentionConfigError::InvalidValue {
                        field: "artefact_directories",
                        ..
                    })
                ),
                "{directory:?} should not escape one directory component"
            );
        }

        for directory in [
            ".aethyme", ".git", "SRC", "lib", "tests", "docs", "examples",
        ] {
            let mut policy = RetentionPolicy::default();
            policy.artefact_directories = vec![directory.into()];
            assert!(
                matches!(
                    policy.validate(),
                    Err(RetentionConfigError::InvalidValue {
                        field: "artefact_directories",
                        ..
                    })
                ),
                "{directory:?} should remain outside the configurable artifact catalog"
            );
        }
    }

    /// The built-in catalog and the protected list describe the same
    /// directories from opposite sides, and nothing else keeps them agreeing.
    /// A built-in artefact that is also protected would make the two contradict
    /// each other; an unrecoverable data root that is not protected is exactly
    /// the loss the fixed catalog was written to prevent.
    #[test]
    fn protected_names_cover_unrecoverable_roots_without_contradicting_the_catalog() {
        for name in ["target", "node_modules", ".venv", "build", "dist"] {
            assert!(
                crate::is_artefact_directory(name),
                "{name} must stay in the built-in artefact catalog"
            );
            assert!(
                is_safe_artefact_directory_name(name),
                "{name} is a built-in artefact and must not also be protected"
            );
        }

        // Ignored, large, and not rebuildable from the repository -- the
        // combination that makes an operator want to list them and makes the
        // removal permanent.
        for name in ["data", "fixtures", "logs", "coverage"] {
            assert!(
                !crate::is_artefact_directory(name),
                "{name} must not be a built-in artefact"
            );
            assert!(
                !is_safe_artefact_directory_name(name),
                "{name} must stay outside the configurable artefact catalog"
            );
        }
    }

    #[test]
    fn retention_field_allowlist_matches_serialized_policy_keys() {
        let serialized = toml::Value::try_from(RetentionPolicy::default()).unwrap();
        let serialized_fields = serialized
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let allowlisted_fields = RETENTION_POLICY_FIELDS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        assert_eq!(
            serialized_fields, allowlisted_fields,
            "retention parsing allowlist must stay in lockstep with RetentionPolicy"
        );
    }

    #[test]
    fn unknown_fields_warn_and_known_values_still_apply() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(BROKER_CONFIG_RELPATH),
            "[retention]\nartifact_sweep_budget_ms = 0\nroutine_size_budget_ms = 0\nfuture_sweep_days = 14\n",
        )
        .unwrap();

        let report = load_retention_policy_report(repo.path()).unwrap();
        assert_eq!(report.policy.artifact_sweep_budget_ms, 0);
        assert_eq!(report.policy.routine_size_budget_ms, 0);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].field, "retention.future_sweep_days");
        assert!(
            report.warnings[0]
                .message
                .contains("known retention settings remain active")
        );
        assert_eq!(load_retention_policy(repo.path()).unwrap(), report.policy);
    }

    #[test]
    fn unknown_top_level_fields_use_broker_configuration_warning() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(BROKER_CONFIG_RELPATH),
            "future_broker_setting = true\n[retention]\n",
        )
        .unwrap();

        let report = load_retention_policy_report(repo.path()).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].field, "broker.future_broker_setting");
        assert!(
            report.warnings[0]
                .message
                .contains("unknown broker configuration field")
        );
        assert!(!report.warnings[0].message.contains("retention schema 0"));
    }

    #[test]
    fn unsupported_schema_is_checked_before_unknown_fields() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(BROKER_CONFIG_RELPATH),
            "[retention]\nschema_version = 2\nfuture_sweep_days = 14\n",
        )
        .unwrap();

        assert!(matches!(
            load_retention_policy_report(repo.path()),
            Err(RetentionConfigError::UnsupportedSchema {
                found: 2,
                supported: RETENTION_POLICY_SCHEMA_VERSION,
            })
        ));
    }

    #[test]
    fn invalid_known_values_still_fail_closed() {
        let cases = [
            "[retention]\nterminal_events_days = 0\n",
            "[retention]\nstartup_budget_ms = 5001\n",
            "[retention]\nretained_bytes_budget = 1125899906842625\n",
        ];
        for text in cases {
            let repo = tempfile::tempdir().unwrap();
            std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
            std::fs::write(repo.path().join(BROKER_CONFIG_RELPATH), text).unwrap();
            assert!(load_retention_policy(repo.path()).is_err(), "{text}");
        }
    }
}
