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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionPolicy {
    pub schema_version: u32,
    pub terminal_events_days: u32,
    pub gate_results_days: u32,
    pub terminal_merge_queue_days: u32,
    pub command_metrics_days: u32,
    pub closed_worktrees_days: u32,
    /// Soft repository storage budget. `0` disables budget warnings.
    pub retained_bytes_budget: u64,
    /// Idle days before a closed session's build caches are reclaimed without
    /// confirmation. Deliberately longer than `closed_worktrees_days`: a
    /// represented worktree is removed whole at that shorter age, so this
    /// governs the worktrees cleanup refuses to touch, where a resumed session
    /// would otherwise pay for a full cold rebuild.
    pub artifact_reclaim_days: u32,
    pub orphan_worktree_roots_days: u32,
    /// Wall-clock budget for the autonomous artifact sweep. `0` disables it.
    pub artifact_sweep_budget_ms: u64,
    /// Minimum spacing between autonomous artifact sweeps.
    pub artifact_sweep_interval_hours: u32,
    pub startup_budget_ms: u64,
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
            retained_bytes_budget: 1_073_741_824,
            artifact_reclaim_days: 14,
            orphan_worktree_roots_days: 1,
            artifact_sweep_budget_ms: 0,
            artifact_sweep_interval_hours: 24,
            startup_budget_ms: 25,
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

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BrokerConfig {
    retention: RetentionPolicy,
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

pub fn load_retention_policy(repo: &Path) -> Result<RetentionPolicy, RetentionConfigError> {
    let path = repo.join(BROKER_CONFIG_RELPATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RetentionPolicy::default());
        }
        Err(source) => return Err(RetentionConfigError::Io { path, source }),
    };
    let config: BrokerConfig = toml::from_str(&text)
        .map_err(|error: toml::de::Error| RetentionConfigError::Parse(error.to_string()))?;
    config.retention.validate()?;
    Ok(config.retention)
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

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GcPlan {
    pub schema_version: u32,
    pub digest: String,
    pub evaluated_at: i64,
    pub policy: RetentionPolicy,
    pub rows: Vec<GcRowCandidate>,
    pub files: Vec<GcFileCandidate>,
    pub worktrees: Vec<GcWorktreeCandidate>,
    pub artifacts: Vec<GcArtifactCandidate>,
    pub orphans: Vec<GcOrphanCandidate>,
    pub blockers: Vec<GcBlocker>,
    pub estimated_reclaimable_bytes: u64,
    /// Every byte held by retained worktrees, whether or not this plan acts on
    /// it. Reporting only: excluded from the digest so measured sizes never
    /// invalidate an authorization.
    pub estimated_retained_bytes: u64,
    /// Bytes this plan deliberately leaves in place because a retention or
    /// provenance gate blocked them.
    pub estimated_blocked_bytes: u64,
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
    pub reclaimed_bytes: u64,
    pub failures: Vec<String>,
    pub recovery_action: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GcHealth {
    pub policy: RetentionPolicy,
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
    pub blockers: usize,
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
        })?;
        self.digest = format!("{:x}", Sha256::digest(bytes));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_conservative_bounded_defaults() {
        let repo = tempfile::tempdir().unwrap();
        let policy = load_retention_policy(repo.path()).unwrap();
        assert_eq!(policy, RetentionPolicy::default());
        assert!(policy.terminal_events_days >= policy.gate_results_days);
        assert!(policy.terminal_merge_queue_days >= policy.gate_results_days);
        assert!(policy.startup_budget_ms <= 25);
        assert_eq!(policy.artifact_sweep_budget_ms, 0);
        assert_eq!(policy.retained_bytes_budget, 1_073_741_824);
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
    fn unknown_fields_versions_and_unbounded_values_fail_closed() {
        let cases = [
            "[retention]\nunknown = 1\n",
            "[retention]\nschema_version = 2\n",
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
