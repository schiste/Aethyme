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
    pub blockers: Vec<GcBlocker>,
    pub estimated_reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GcApplyReport {
    pub digest: String,
    pub complete: bool,
    pub deadline_reached: bool,
    pub rows_removed: usize,
    pub files_completed: Vec<String>,
    pub sessions_cleaned: Vec<i64>,
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
    pub estimated_reclaimable_bytes: u64,
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
            blockers: &'a [GcBlocker],
        }
        let bytes = serde_json::to_vec(&Authorization {
            schema_version: self.schema_version,
            policy: &self.policy,
            rows: &self.rows,
            files: &self.files,
            worktrees: &self.worktrees,
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
        ];
        for text in cases {
            let repo = tempfile::tempdir().unwrap();
            std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
            std::fs::write(repo.path().join(BROKER_CONFIG_RELPATH), text).unwrap();
            assert!(load_retention_policy(repo.path()).is_err(), "{text}");
        }
    }
}
