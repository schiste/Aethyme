//! The route table (#1214): which hostname reaches which environment
//! component.
//!
//! Every routed component of a live environment is reachable as
//! `<component>.<namespace>.<route_domain>`, and the profile's first routed
//! component also as `<namespace>.<route_domain>`. A route names its
//! upstream port only while the environment is `active`, and that port is
//! always the one its reservation lease allocated, so a route never points
//! at a port the environment does not hold.
//!
//! A shared environment's serving slot also answers on a stable alias host
//! (#1274), recorded in its [`ROUTE_ALIAS_FILE`]. The `serving` owner pointer
//! selects which slot's alias belongs in a route-table snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::compile::EnvironmentOwner;
use super::lifecycle::{RESERVATION_GRANT, read_grant};
use super::spec::Runtime;
use super::store::{EnvironmentPhase, EnvironmentRecord, EnvironmentStore, StoreError};

const SERVING_SNAPSHOT_ATTEMPTS: usize = 3;

/// In an environment's directory: the alias host its first routed
/// component also answers on, while it is a shared environment's serving
/// slot.
pub const ROUTE_ALIAS_FILE: &str = "route-alias";

/// The alias host recorded for `env_id`, if any.
pub fn route_alias(root: &Path, env_id: &str) -> Option<String> {
    std::fs::read_to_string(root.join(env_id).join(ROUTE_ALIAS_FILE))
        .ok()
        .map(|alias| alias.trim().to_ascii_lowercase())
        .filter(|alias| !alias.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Route {
    pub host: String,
    pub env_id: String,
    pub namespace: String,
    pub component: String,
    /// The environment's phase (`active`, `parked`, ...).
    pub state: &'static str,
    /// Loopback port to forward to; present only while `active`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<u16>,
    /// Directory served for a `static` component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_dir: Option<PathBuf>,
    /// Health timeout: how long a request may be held while this wakes.
    pub wake_timeout_seconds: u64,
}

pub fn route_table(store: &EnvironmentStore, root: &Path) -> Result<Vec<Route>, StoreError> {
    route_table_with_observer(store, root, || {}, || {})
}

#[cfg(test)]
pub(super) fn route_table_with_test_hook(
    store: &EnvironmentStore,
    root: &Path,
    hook: impl FnMut(),
) -> Result<Vec<Route>, StoreError> {
    route_table_with_observer(store, root, || {}, hook)
}

#[cfg(test)]
pub(super) fn route_table_with_list_hook(
    store: &EnvironmentStore,
    root: &Path,
    hook: impl FnMut(),
) -> Result<Vec<Route>, StoreError> {
    route_table_with_observer(store, root, hook, || {})
}

fn route_table_with_observer(
    store: &EnvironmentStore,
    root: &Path,
    mut after_initial_list: impl FnMut(),
    mut after_pointer_read: impl FnMut(),
) -> Result<Vec<Route>, StoreError> {
    for _ in 0..SERVING_SNAPSHOT_ATTEMPTS {
        if let Some(routes) = route_table_snapshot(
            store,
            root,
            &mut after_initial_list,
            &mut after_pointer_read,
        )? {
            return Ok(routes);
        }
    }
    Err(StoreError::Corrupt(format!(
        "shared serving selection changed during {SERVING_SNAPSHOT_ATTEMPTS} route snapshots; retry after the redeploy settles"
    )))
}

fn route_table_snapshot(
    store: &EnvironmentStore,
    root: &Path,
    after_initial_list: &mut impl FnMut(),
    after_pointer_read: &mut impl FnMut(),
) -> Result<Option<Vec<Route>>, StoreError> {
    let initial_records = store.list(None)?;
    let initial_groups = shared_route_groups(&initial_records)?;
    after_initial_list();
    let mut snapshots = Vec::with_capacity(initial_groups.len());
    let mut deferred_errors = Vec::new();

    for group in initial_groups.values() {
        let default_owner = group.default_owner();
        let snapshot = super::shared::serving_pointer_snapshot(
            root,
            &group.repository,
            &group.shared_name,
            default_owner.as_deref().ok(),
        )
        .map_err(|error| {
            StoreError::Corrupt(format!(
                "could not read serving pointer for shared environment {:?}: {error}",
                group.shared_name
            ))
        })?;
        after_pointer_read();
        snapshots.push((group, snapshot));
    }

    // The pointer can be published after the initial database list sees the
    // old slot but before its atomic rename. Refresh the complete table after
    // capturing pointers and retry if any shared owner set changed; otherwise
    // the new pointer could select an owner absent from the canonical routes.
    let records = store.list(None)?;
    let groups = shared_route_groups(&records)?;
    if groups != initial_groups {
        return Ok(None);
    }
    let mut routes: Vec<Route> = records
        .iter()
        .filter(|record| record.phase.retains_worktree())
        .flat_map(|record| record_routes(record, root, None))
        .collect();

    for (group, snapshot) in &snapshots {
        let owner = match snapshot.owner() {
            Some(owner) if group.owners.contains(owner) => owner.to_string(),
            Some(owner) => {
                // A new owner may appear just after the refreshed list. Re-list
                // only on this exceptional path and rebuild if its owner set
                // moved; a stable unknown pointer remains a corruption error.
                let latest_groups = shared_route_groups(&store.list(None)?)?;
                if latest_groups != groups {
                    return Ok(None);
                }
                deferred_errors.push(StoreError::Corrupt(format!(
                    "serving pointer for shared environment {:?} contains unknown owner {owner:?}",
                    group.shared_name
                )));
                continue;
            }
            None => match group.default_owner() {
                Ok(owner) => owner.to_string(),
                Err(error) => {
                    deferred_errors.push(error);
                    continue;
                }
            },
        };

        // The pointer chooses the slot. Re-read its record and grant only
        // after capturing the pointer generation; the initial route-table
        // list may have observed that slot while it was still starting.
        match current_serving_record(store, &records, group, &owner, snapshot.exists()) {
            Ok(Some(record)) => match route_alias(root, &record.env_id) {
                Some(alias) => {
                    let alias = alias.to_ascii_lowercase();
                    if let Some(route) = record_routes(&record, root, Some(&alias))
                        .into_iter()
                        .find(|route| route.host == alias)
                    {
                        routes.push(route);
                    } else if snapshot.exists() {
                        deferred_errors.push(StoreError::Corrupt(format!(
                            "serving slot for shared environment {:?} has no routed component for its alias",
                            group.shared_name
                        )));
                    }
                }
                None => {}
            },
            Ok(None) => {}
            Err(error) => deferred_errors.push(error),
        }
    }

    // A rename during any group's read invalidates the whole table. Check
    // after all groups so later work cannot leave an earlier group stale.
    for (group, snapshot) in &snapshots {
        let current = snapshot
            .is_current(root, &group.repository, &group.shared_name)
            .map_err(|error| {
                StoreError::Corrupt(format!(
                    "could not verify serving pointer for shared environment {:?}: {error}",
                    group.shared_name
                ))
            })?;
        if !current {
            return Ok(None);
        }
    }
    if let Some(error) = deferred_errors.into_iter().next() {
        return Err(error);
    }

    routes.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(Some(routes))
}

#[derive(Debug, PartialEq, Eq)]
struct SharedRouteGroup {
    repository: String,
    shared_name: String,
    owners: BTreeSet<String>,
}

impl SharedRouteGroup {
    fn default_owner(&self) -> Result<&str, StoreError> {
        let mut owners = self.owners.iter().map(String::as_str);
        match (owners.next(), owners.next()) {
            (Some(owner), None) => Ok(owner),
            _ => Err(StoreError::Corrupt(format!(
                "shared environment {:?} has no unique owner for an absent serving pointer",
                self.shared_name
            ))),
        }
    }
}

fn shared_route_groups(
    records: &[EnvironmentRecord],
) -> Result<BTreeMap<(String, String), SharedRouteGroup>, StoreError> {
    let mut groups: BTreeMap<(String, String), SharedRouteGroup> = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.phase.retains_worktree())
    {
        let EnvironmentOwner::Ref { name } = &record.owner else {
            continue;
        };
        let Some((shared_name, reference)) = name.split_once('@') else {
            return Err(StoreError::Corrupt(format!(
                "shared environment owner {name:?} has no reference separator"
            )));
        };
        if shared_name.is_empty() || reference.is_empty() {
            return Err(StoreError::Corrupt(format!(
                "shared environment owner {name:?} is incomplete"
            )));
        }
        let key = (record.repository.clone(), shared_name.to_string());
        groups
            .entry(key)
            .or_insert_with(|| SharedRouteGroup {
                repository: record.repository.clone(),
                shared_name: shared_name.to_string(),
                owners: BTreeSet::new(),
            })
            .owners
            .insert(name.clone());
    }
    Ok(groups)
}

fn current_serving_record(
    store: &EnvironmentStore,
    records: &[EnvironmentRecord],
    group: &SharedRouteGroup,
    owner: &str,
    pointer_exists: bool,
) -> Result<Option<EnvironmentRecord>, StoreError> {
    let mut matches: Vec<String> = records
        .iter()
        .filter(|record| {
            record.repository == group.repository
                && record.phase.retains_worktree()
                && matches!(&record.owner, EnvironmentOwner::Ref { name } if name == owner)
        })
        .map(|record| record.env_id.clone())
        .collect();
    let missing_pointer_fallback = !pointer_exists
        && group
            .default_owner()
            .is_ok_and(|default_owner| default_owner == owner);
    if matches.len() != 1 && !(matches.is_empty() && missing_pointer_fallback) {
        matches = store
            .list(Some(&group.repository))?
            .into_iter()
            .filter(|record| {
                record.phase.retains_worktree()
                    && matches!(&record.owner, EnvironmentOwner::Ref { name } if name == owner)
            })
            .map(|record| record.env_id)
            .collect();
    }
    let env_id = match matches.as_slice() {
        [env_id] => env_id,
        [] if missing_pointer_fallback => return Ok(None),
        [] => {
            return Err(StoreError::Corrupt(format!(
                "serving pointer for shared environment {:?} names {owner:?}, but no live record exists",
                group.shared_name
            )));
        }
        _ => {
            return Err(StoreError::Corrupt(format!(
                "serving pointer for shared environment {:?} names {owner:?}, but multiple live records match",
                group.shared_name
            )));
        }
    };
    let Some(record) = store.get(env_id)? else {
        return Err(StoreError::NotFound(env_id.clone()));
    };
    if record.repository != group.repository
        || !matches!(&record.owner, EnvironmentOwner::Ref { name } if name == owner)
        || !record.phase.retains_worktree()
    {
        return Err(StoreError::Corrupt(format!(
            "serving record {env_id} changed while its route snapshot was built"
        )));
    }
    Ok(Some(record))
}

fn record_routes(record: &EnvironmentRecord, root: &Path, alias: Option<&str>) -> Vec<Route> {
    let Some(namespace) = record.namespace.clone() else {
        return Vec::new();
    };
    let grant = read_grant(root, &record.env_id, RESERVATION_GRANT);
    let domain = &record.snapshot.route_domain;
    let wake_timeout_seconds = record
        .snapshot
        .components
        .iter()
        .filter_map(|component| {
            component
                .health
                .as_ref()
                .map(|health| health.timeout_seconds)
        })
        .sum::<u64>()
        .max(30);
    let mut first = true;
    let mut routes = Vec::new();
    for component in record.snapshot.components.iter().filter(|c| c.routed) {
        let upstream = (record.phase == EnvironmentPhase::Active
            && component.runtime != Runtime::Static)
            .then(|| {
                grant.as_ref().and_then(|grant| {
                    grant
                        .lease
                        .allocations
                        .iter()
                        .find(|allocation| Some(&allocation.key) == component.port_key.as_ref())
                        .and_then(|allocation| allocation.value.parse().ok())
                })
            })
            .flatten();
        let static_dir = (component.runtime == Runtime::Static)
            .then(|| {
                record
                    .worktree_root
                    .as_ref()
                    .zip(component.serve_dir.as_ref())
                    .map(|(root, dir)| Path::new(root).join(dir))
            })
            .flatten();
        let mut hosts = vec![format!("{}.{namespace}.{domain}", component.name)];
        if first {
            hosts.push(format!("{namespace}.{domain}"));
            if let Some(alias) = alias {
                hosts.push(alias.to_string());
            }
            first = false;
        }
        for host in hosts {
            routes.push(Route {
                host: host.to_ascii_lowercase(),
                env_id: record.env_id.clone(),
                namespace: namespace.clone(),
                component: component.name.clone(),
                state: record.phase.as_str(),
                upstream,
                static_dir: static_dir.clone(),
                wake_timeout_seconds,
            });
        }
    }
    routes
}

/// A stable tag for the table's content, for `ETag` / `If-None-Match`.
pub fn etag(routes: &[Route]) -> String {
    let bytes = serde_json::to_vec(routes).unwrap_or_default();
    format!("\"{}\"", &format!("{:x}", Sha256::digest(bytes))[..16])
}

/// The route for a `Host` header value: case-insensitive, port ignored.
pub fn lookup<'a>(routes: &'a [Route], host: &str) -> Option<&'a Route> {
    let host = host.trim().to_ascii_lowercase();
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host.as_str(), |(name, _)| name);
    let host = host.trim_end_matches('.');
    routes.iter().find(|route| route.host == host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(owners: &[&str]) -> SharedRouteGroup {
        SharedRouteGroup {
            repository: "repo".into(),
            shared_name: "baseline".into(),
            owners: owners.iter().map(|owner| (*owner).to_string()).collect(),
        }
    }

    #[test]
    fn absent_pointer_fallback_requires_a_unique_exact_owner() {
        let ref_ending_in_slot_suffix = group(&["baseline@refs/heads/feature#b"]);
        assert_eq!(
            ref_ending_in_slot_suffix.default_owner().unwrap(),
            "baseline@refs/heads/feature#b"
        );

        let blue_green_pair = group(&[
            "baseline@refs/heads/feature",
            "baseline@refs/heads/feature#b",
        ]);
        assert!(blue_green_pair.default_owner().is_err());

        let ref_ending_in_slot_suffix_pair = group(&[
            "baseline@refs/heads/feature#b",
            "baseline@refs/heads/feature#b#b",
        ]);
        assert!(ref_ending_in_slot_suffix_pair.default_owner().is_err());

        let ambiguous_refs = group(&["baseline@refs/heads/feature", "baseline@refs/heads/other"]);
        assert!(ambiguous_refs.default_owner().is_err());
    }
}
