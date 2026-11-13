//! Publication policy and the remote checks a first enrollment reuses.
//!
//! `aethyme deploy` publishes its verified commit without promotion (see
//! `enrollment_publication`), reusing this module's publication-policy,
//! remote verification and local-main checks. Local promotion and the
//! publication of a promoted integration prefix were removed: work lands
//! through each session's own pull request.
//!
//! The same module owns the committed `[delivery]` and `[publication]`
//! configuration tables, which `broker push` and local acceptance read.

use crate::broker::{Broker, BrokerOpError};
use crate::git::GitRepo;
use crate::operations::{CoordinatedOperationReport, UnknownOutcomeRecovery};
use std::path::Path;

pub const PUBLICATION_POLICY_SCHEMA_VERSION: u32 = 1;
pub const REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryDeliveryConfigTable {
    /// Retired route selector (`local_main_merge` or `pull_request`) of the
    /// removed `broker ship` command. Existing configurations still parse;
    /// the value is ignored because pull requests are the only landing path.
    #[serde(default, rename = "default")]
    _retired_default: Option<toml::Value>,
    #[serde(default)]
    push_session_branches: bool,
}

/// Typed `[delivery]` configuration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryDeliveryConfig {
    pub schema_version: u32,
    /// Durable, repository-level authorization for `broker push`: a session
    /// may publish its own `agent/*` branch, and nothing else, without a
    /// per-command `--reason`. Absent means false.
    pub push_session_branches: bool,
}

impl RepositoryDeliveryConfig {
    pub fn from_config_text(text: &str) -> Result<Option<Self>, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(delivery) = value.get("delivery") else {
            return Ok(None);
        };
        let table: RepositoryDeliveryConfigTable = delivery
            .clone()
            .try_into()
            .map_err(|error| format!("[delivery] policy is invalid: {error}"))?;
        Ok(Some(Self {
            schema_version: REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION,
            push_session_branches: table.push_session_branches,
        }))
    }

    pub fn load(main_root: &Path) -> Result<Option<Self>, String> {
        // `.aethyme/config.toml`, `.yaml` or `.yml` (#1223), as TOML text.
        let Some((_, text)) =
            aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
                .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        Self::from_config_text(&text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ShipPublicationMode {
    #[default]
    Direct,
    ReviewGated,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShipPublicationPolicy {
    pub schema_version: u32,
    pub mode: ShipPublicationMode,
    /// Retired: it authorized `broker advanced ship execute --break-glass`,
    /// which was deleted with the ship commands. Existing configurations still
    /// parse; the value is ignored and `broker status` reports
    /// `config.allow-break-glass-removed` so the operator can delete it.
    #[serde(rename = "allow_break_glass", skip_serializing)]
    pub retired_allow_break_glass: Option<bool>,
}

impl Default for ShipPublicationPolicy {
    fn default() -> Self {
        Self {
            schema_version: PUBLICATION_POLICY_SCHEMA_VERSION,
            mode: ShipPublicationMode::Direct,
            retired_allow_break_glass: None,
        }
    }
}

/// Whether fast-forwarding the primary checkout's default branch is safe,
/// and the local changes that decided it.
#[derive(Debug, Clone)]
pub(crate) struct LocalMainSyncAssessment {
    /// On the expected branch at the expected head, a fast-forward, with no
    /// tracked change and no untracked path the incoming commits would hit.
    pub(crate) safe: bool,
    pub(crate) tracked_dirty_paths: Vec<String>,
    pub(crate) untracked_paths: Vec<String>,
}

pub(crate) fn ship_operation_failure(
    phase: &'static str,
    report: &CoordinatedOperationReport,
) -> BrokerOpError {
    if report.operation.status == crate::OperationStatus::OutcomeUnknown {
        BrokerOpError::CoordinatedOperationBlocked {
            repository: report.operation.repository.clone(),
            operation_id: report.operation.id,
            recovery: UnknownOutcomeRecovery::from_operation(&report.operation),
        }
    } else {
        BrokerOpError::ShipOperationFailed {
            phase,
            operation_id: report.operation.id,
            status: report.operation.status.as_str(),
        }
    }
}

pub(crate) fn publication_policy_at(
    broker: &Broker,
    policy_source_commit: &str,
) -> Result<ShipPublicationPolicy, BrokerOpError> {
    if broker
        .repo_handle()
        .resolve_ref(policy_source_commit)
        .is_none()
    {
        return Err(BrokerOpError::ShipPlanUnavailable {
            what: "trusted publication policy",
            reason: format!(
                "the remote default SHA {policy_source_commit} is not available locally; fetch it before planning publication"
            ),
        });
    }
    let Some(text) = broker
        .repo_handle()
        .config_toml_at_commit(policy_source_commit, "config")?
    else {
        return Ok(ShipPublicationPolicy::default());
    };
    ShipPublicationPolicy::from_config_text(&text)
}

impl ShipPublicationPolicy {
    /// The same parser used by ordinary committed publication policy.
    pub fn from_config_text(text: &str) -> Result<Self, BrokerOpError> {
        let value =
            text.parse::<toml::Value>()
                .map_err(|error| BrokerOpError::ShipPublicationPolicy {
                    reason: format!("committed .aethyme/config.toml is invalid: {error}"),
                    remediation: "fix and submit the committed publication policy".into(),
                })?;
        let Some(publication) = value.get("publication") else {
            return Ok(ShipPublicationPolicy::default());
        };
        let policy: ShipPublicationPolicy = publication.clone().try_into().map_err(|error| {
            BrokerOpError::ShipPublicationPolicy {
                reason: format!("committed [publication] policy is invalid: {error}"),
                remediation: "fix and submit the committed publication policy".into(),
            }
        })?;
        if policy.schema_version != PUBLICATION_POLICY_SCHEMA_VERSION {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: format!(
                    "publication policy schema {} is unsupported; expected {}",
                    policy.schema_version, PUBLICATION_POLICY_SCHEMA_VERSION
                ),
                remediation: "upgrade Aethyme or use a supported committed policy schema".into(),
            });
        }
        Ok(policy)
    }
}

pub(crate) fn assess_local_main_sync_repo(
    repo: &GitRepo,
    default_branch: &str,
    default_branch_ref: &str,
    expected_head: &str,
    confirm: &str,
) -> Result<LocalMainSyncAssessment, String> {
    let current_branch = repo.current_branch().map_err(|error| error.to_string())?;
    let current_head = repo.head_commit().map_err(|error| error.to_string())?;
    let current_ref = repo
        .resolve_ref(default_branch_ref)
        .ok_or_else(|| format!("{default_branch_ref} no longer resolves"))?;
    let mut tracked_dirty_paths = repo
        .tracked_dirty_paths()
        .map_err(|error| error.to_string())?;
    let mut untracked_paths = repo.untracked_paths().map_err(|error| error.to_string())?;
    tracked_dirty_paths.sort();
    tracked_dirty_paths.dedup();
    untracked_paths.sort();
    untracked_paths.dedup();
    let fast_forward = repo.is_ancestor(&current_head, confirm);
    let incoming_paths = if fast_forward {
        repo.changed_between(&current_head, confirm)
            .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };
    let mut conflicting_untracked_paths = untracked_paths
        .iter()
        .filter(|untracked| {
            incoming_paths
                .iter()
                .any(|incoming| checkout_paths_collide(untracked, incoming))
        })
        .cloned()
        .collect::<Vec<_>>();
    conflicting_untracked_paths.sort();
    conflicting_untracked_paths.dedup();
    let current_branch_matches = current_branch == default_branch;
    let local_head_unchanged = current_head == expected_head && current_ref == expected_head;
    let safe = current_branch_matches
        && local_head_unchanged
        && fast_forward
        && tracked_dirty_paths.is_empty()
        && conflicting_untracked_paths.is_empty();
    Ok(LocalMainSyncAssessment {
        safe,
        tracked_dirty_paths,
        untracked_paths,
    })
}

fn checkout_paths_collide(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || b.strip_prefix(a)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn ls_remote_sha(output: &str, ref_name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == ref_name
            && sha.len() == 40
            && sha.chars().all(|character| character.is_ascii_hexdigit()))
        .then(|| sha.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION, RepositoryDeliveryConfig, ShipPublicationMode,
        ShipPublicationPolicy, checkout_paths_collide,
    };

    #[test]
    fn checkout_collision_covers_exact_and_file_directory_replacements() {
        assert!(checkout_paths_collide("feature.txt", "feature.txt"));
        assert!(checkout_paths_collide("feature/nested.txt", "feature"));
        assert!(checkout_paths_collide("feature", "feature/nested.txt"));
        assert!(!checkout_paths_collide(".codex/local.md", "src/main.rs"));
        assert!(!checkout_paths_collide("feature-old", "feature"));
    }

    #[test]
    fn delivery_config_is_strict_about_unknown_keys() {
        assert_eq!(
            RepositoryDeliveryConfig::from_config_text("schema = 1\n").unwrap(),
            None
        );
        let config = RepositoryDeliveryConfig::from_config_text("[delivery]\n")
            .unwrap()
            .expect("delivery policy");
        assert!(!config.push_session_branches);
        assert_eq!(
            config.schema_version,
            REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION
        );
        assert!(
            RepositoryDeliveryConfig::from_config_text(
                "[delivery]\npush_session_branches = true\nallow_force = true\n"
            )
            .is_err()
        );
    }

    /// The retired `default` route selector of the removed `broker ship`
    /// command must not make an existing configuration fail to load, and it
    /// must not change what the table authorizes.
    #[test]
    fn delivery_config_ignores_the_retired_route_selector() {
        for value in [
            "\"pull_request\"",
            "\"local_main_merge\"",
            "\"reset_hard\"",
            "3",
        ] {
            let config = RepositoryDeliveryConfig::from_config_text(&format!(
                "[delivery]\ndefault = {value}\npush_session_branches = true\n"
            ))
            .unwrap()
            .expect("delivery policy");
            assert!(config.push_session_branches, "default = {value}");
        }
        let config =
            RepositoryDeliveryConfig::from_config_text("[delivery]\ndefault = \"pull_request\"\n")
                .unwrap()
                .expect("delivery policy");
        assert!(!config.push_session_branches);
    }

    /// A repository can authorize session-branch pushes on its own.
    #[test]
    fn delivery_config_accepts_push_session_branches() {
        let config = RepositoryDeliveryConfig::from_config_text(
            "[delivery]\npush_session_branches = true\n",
        )
        .unwrap()
        .expect("delivery policy");
        assert!(config.push_session_branches);
        assert!(
            RepositoryDeliveryConfig::from_config_text(
                "[delivery]\npush_session_branches = \"yes\"\n"
            )
            .is_err()
        );
    }

    /// `allow_break_glass` gated the deleted `ship execute --break-glass`.
    /// An old configuration still loads, the value changes nothing, and it
    /// never reappears in serialized policy.
    #[test]
    fn publication_policy_parses_and_ignores_the_retired_break_glass_key() {
        for value in ["true", "false"] {
            let policy = ShipPublicationPolicy::from_config_text(&format!(
                "[publication]\nmode = \"review_gated\"\nallow_break_glass = {value}\n"
            ))
            .unwrap();
            assert_eq!(policy.mode, ShipPublicationMode::ReviewGated);
            assert_eq!(
                policy.retired_allow_break_glass,
                Some(value == "true"),
                "allow_break_glass = {value}"
            );
            let json = serde_json::to_value(&policy).unwrap();
            assert!(json.get("allow_break_glass").is_none(), "{json}");
            assert!(json.get("retired_allow_break_glass").is_none(), "{json}");
        }
        let policy =
            ShipPublicationPolicy::from_config_text("[publication]\nmode = \"direct\"\n").unwrap();
        assert_eq!(policy.retired_allow_break_glass, None);
    }
}
