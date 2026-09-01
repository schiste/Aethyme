//! Redacted, read-only lease metadata for external routing adapters.
//!
//! The export is an allowlist projection. It deliberately excludes session
//! tasks, worktree and host paths, remote URLs, commands, and ownership tokens.

use crate::{
    BrokerError, BrokerStore, GitError, GitRepo, Lease, LeaseKind, RemoteTargetError, SessionStatus,
};
use std::collections::{BTreeMap, BTreeSet};

pub const LEASE_ROUTING_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_LEASE_ROUTING_EXPORT_LIMIT: usize = 200;
pub const MAX_LEASE_ROUTING_EXPORT_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseExportState {
    Active,
    Expired,
    Released,
    InactiveOwner,
}

impl LeaseExportState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Released => "released",
            Self::InactiveOwner => "inactive_owner",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseExportPathKind {
    Exact,
    Directory,
}

impl LeaseExportPathKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Directory => "directory",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseExportConflictState {
    None,
    Overlapping,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseExportRepository {
    pub coordination_key: String,
    pub display_slug: String,
    pub remote_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseExportSelector {
    pub session_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_entry_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseRoutingConfiguration {
    pub source_commit: String,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseRoutingItem {
    pub lease_id: i64,
    pub owner_session_id: i64,
    pub path: String,
    pub path_kind: LeaseExportPathKind,
    pub lease_kind: LeaseKind,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub state: LeaseExportState,
    pub conflict_state: LeaseExportConflictState,
    pub conflicting_session_ids: Vec<i64>,
    pub routing_categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseRoutingExport {
    pub schema_version: u32,
    pub source_time: i64,
    pub repository: LeaseExportRepository,
    pub selector: LeaseExportSelector,
    pub routing_configuration: LeaseRoutingConfiguration,
    pub leases: Vec<LeaseRoutingItem>,
    pub total_matching: usize,
    pub limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseRoutingExportOptions {
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub limit: usize,
}

impl Default for LeaseRoutingExportOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            queue_entry_id: None,
            limit: DEFAULT_LEASE_ROUTING_EXPORT_LIMIT,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseRoutingExportError {
    #[error(transparent)]
    Store(#[from] BrokerError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Remote(#[from] RemoteTargetError),
    #[error("lease export requires --session <id> or --entry <id>")]
    InvalidSelector,
    #[error("queue entry {entry_id} was not found")]
    QueueEntryNotFound { entry_id: i64 },
    #[error(
        "queue entry {entry_id} belongs to session {actual_session_id}, not requested session {requested_session_id}"
    )]
    SelectorMismatch {
        entry_id: i64,
        actual_session_id: i64,
        requested_session_id: i64,
    },
    #[error("lease export --limit must be between 1 and {MAX_LEASE_ROUTING_EXPORT_LIMIT}")]
    InvalidLimit,
    #[error("cannot resolve a canonical repository remote: {reason}")]
    RepositoryIdentity { reason: String },
    #[error("invalid committed [leases.routing] configuration: {reason}")]
    InvalidRoutingConfiguration { reason: String },
}

pub(crate) fn build_lease_routing_export(
    repo: &GitRepo,
    store: &BrokerStore,
    options: LeaseRoutingExportOptions,
    source_time: i64,
) -> Result<LeaseRoutingExport, LeaseRoutingExportError> {
    if options.limit == 0 || options.limit > MAX_LEASE_ROUTING_EXPORT_LIMIT {
        return Err(LeaseRoutingExportError::InvalidLimit);
    }
    let (session_id, queue_entry_id) = match (options.session_id, options.queue_entry_id) {
        (Some(session_id), None) => (session_id, None),
        (None, Some(entry_id)) => {
            let entry = store
                .merge_queue_entry(entry_id)
                .map_err(|error| match error {
                    BrokerError::SessionNotFound(_) => {
                        LeaseRoutingExportError::QueueEntryNotFound { entry_id }
                    }
                    other => LeaseRoutingExportError::Store(other),
                })?;
            (entry.session_id, Some(entry_id))
        }
        (Some(requested_session_id), Some(entry_id)) => {
            let entry = store.merge_queue_entry(entry_id)?;
            if entry.session_id != requested_session_id {
                return Err(LeaseRoutingExportError::SelectorMismatch {
                    entry_id,
                    actual_session_id: entry.session_id,
                    requested_session_id,
                });
            }
            (requested_session_id, Some(entry_id))
        }
        (None, None) => return Err(LeaseRoutingExportError::InvalidSelector),
    };

    let session = store.session(session_id)?;
    let active_leases = store.active_leases_at(source_time)?;
    let remote = resolve_repository_remote(repo)?;
    let target = repo.resolve_remote_target(&remote, None)?;
    let source_commit = repo.head_commit()?;
    let routing = load_routing_configuration(repo, &source_commit)?;

    let mut leases = store.session_leases(session_id)?;
    leases.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total_matching = leases.len();
    let items = leases
        .into_iter()
        .take(options.limit)
        .map(|lease| {
            project_lease(
                &lease,
                session.status,
                &active_leases,
                &routing.categories,
                source_time,
            )
        })
        .collect();

    Ok(LeaseRoutingExport {
        schema_version: LEASE_ROUTING_EXPORT_SCHEMA_VERSION,
        source_time,
        repository: LeaseExportRepository {
            coordination_key: target.coordination_key,
            display_slug: target.display_slug,
            remote_name: target.remote_name,
        },
        selector: LeaseExportSelector {
            session_id,
            queue_entry_id,
        },
        routing_configuration: LeaseRoutingConfiguration {
            source_commit,
            categories: routing.categories.keys().cloned().collect(),
        },
        leases: items,
        total_matching,
        limit: options.limit,
        truncated: total_matching > options.limit,
    })
}

fn resolve_repository_remote(repo: &GitRepo) -> Result<String, LeaseRoutingExportError> {
    let remotes = repo.remotes()?;
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok("origin".into());
    }
    if remotes.len() == 1 {
        return Ok(remotes[0].clone());
    }
    Err(LeaseRoutingExportError::RepositoryIdentity {
        reason: if remotes.is_empty() {
            "no Git remote is configured".into()
        } else {
            "multiple remotes are configured and none is named origin".into()
        },
    })
}

#[derive(Debug, Default)]
struct RoutingConfiguration {
    categories: BTreeMap<String, Vec<String>>,
}

fn load_routing_configuration(
    repo: &GitRepo,
    source_commit: &str,
) -> Result<RoutingConfiguration, LeaseRoutingExportError> {
    let Some(text) = repo.file_at_commit(source_commit, ".aethyme/config.toml")? else {
        return Ok(RoutingConfiguration::default());
    };
    let value = text.parse::<toml::Value>().map_err(|error| {
        LeaseRoutingExportError::InvalidRoutingConfiguration {
            reason: error.to_string(),
        }
    })?;
    let Some(routing) = value.get("leases").and_then(|leases| leases.get("routing")) else {
        return Ok(RoutingConfiguration::default());
    };
    let table =
        routing
            .as_table()
            .ok_or_else(|| LeaseRoutingExportError::InvalidRoutingConfiguration {
                reason: "leases.routing must be a table of category = [paths] entries".into(),
            })?;
    let mut categories = BTreeMap::new();
    for (category, paths) in table {
        validate_category(category)?;
        let entries = paths.as_array().ok_or_else(|| {
            LeaseRoutingExportError::InvalidRoutingConfiguration {
                reason: format!("category {category:?} must be an array of paths"),
            }
        })?;
        let mut normalized = BTreeSet::new();
        for entry in entries {
            let path = entry.as_str().ok_or_else(|| {
                LeaseRoutingExportError::InvalidRoutingConfiguration {
                    reason: format!("category {category:?} contains a non-string path"),
                }
            })?;
            let path = crate::broker::normalize_lease_path(path).map_err(|error| {
                LeaseRoutingExportError::InvalidRoutingConfiguration {
                    reason: error.to_string(),
                }
            })?;
            normalized.insert(path);
        }
        categories.insert(category.clone(), normalized.into_iter().collect());
    }
    Ok(RoutingConfiguration { categories })
}

fn validate_category(category: &str) -> Result<(), LeaseRoutingExportError> {
    if category.is_empty()
        || category.len() > 64
        || !category
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(LeaseRoutingExportError::InvalidRoutingConfiguration {
            reason: format!(
                "category {category:?} must contain 1..=64 ASCII letters, digits, '.', '_' or '-'"
            ),
        });
    }
    Ok(())
}

fn project_lease(
    lease: &Lease,
    owner_status: SessionStatus,
    active_leases: &[Lease],
    routing: &BTreeMap<String, Vec<String>>,
    source_time: i64,
) -> LeaseRoutingItem {
    let state = lease_state(lease, owner_status, source_time);
    let mut conflicts = if state == LeaseExportState::Active {
        active_leases
            .iter()
            .filter(|other| other.session_id != lease.session_id)
            .filter(|other| crate::leases::paths_overlap(&lease.path, &other.path))
            .map(|other| other.session_id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    conflicts.sort_unstable();
    conflicts.dedup();
    let routing_categories = routing
        .iter()
        .filter(|(_, paths)| {
            paths
                .iter()
                .any(|path| crate::leases::paths_overlap(&lease.path, path))
        })
        .map(|(category, _)| category.clone())
        .collect();
    LeaseRoutingItem {
        lease_id: lease.id,
        owner_session_id: lease.session_id,
        path: lease.path.clone(),
        path_kind: if lease.path.ends_with('/') {
            LeaseExportPathKind::Directory
        } else {
            LeaseExportPathKind::Exact
        },
        lease_kind: lease.kind,
        created_at: lease.created_at,
        expires_at: lease.expires_at,
        state,
        conflict_state: if conflicts.is_empty() {
            LeaseExportConflictState::None
        } else {
            LeaseExportConflictState::Overlapping
        },
        conflicting_session_ids: conflicts,
        routing_categories,
    }
}

fn lease_state(lease: &Lease, owner_status: SessionStatus, source_time: i64) -> LeaseExportState {
    if lease.released_at.is_some() {
        LeaseExportState::Released
    } else if lease.expires_at.is_some_and(|expiry| expiry <= source_time) {
        LeaseExportState::Expired
    } else if matches!(
        owner_status,
        SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
    ) {
        LeaseExportState::Active
    } else {
        LeaseExportState::InactiveOwner
    }
}
