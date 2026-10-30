//! The route table (#1214): which hostname reaches which environment
//! component.
//!
//! Every routed component of a live environment is reachable as
//! `<component>.<namespace>.<route_domain>`, and the profile's first routed
//! component also as `<namespace>.<route_domain>`. A route names its
//! upstream port only while the environment is `active`, and that port is
//! always the one its reservation lease allocated, so a route never points
//! at a port the environment does not hold.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::lifecycle::{RESERVATION_GRANT, read_grant};
use super::spec::Runtime;
use super::store::{EnvironmentPhase, EnvironmentStore, StoreError};

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
    /// Serve `index.html` for extensionless paths that are not files (#1291).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub spa_fallback: bool,
    /// Health timeout: how long a request may be held while this wakes.
    pub wake_timeout_seconds: u64,
}

pub fn route_table(store: &EnvironmentStore, root: &Path) -> Result<Vec<Route>, StoreError> {
    let mut routes = Vec::new();
    for record in store.list(None)? {
        if !record.phase.retains_worktree() {
            continue;
        }
        let Some(namespace) = record.namespace.clone() else {
            continue;
        };
        let grant = read_grant(root, &record.env_id, RESERVATION_GRANT);
        let domain = &record.snapshot.route_domain;
        let wake_timeout_seconds = record
            .snapshot
            .components
            .iter()
            .filter_map(|c| c.health.as_ref().map(|h| h.timeout_seconds))
            .sum::<u64>()
            .max(30);
        let mut first = true;
        for component in record.snapshot.components.iter().filter(|c| c.routed) {
            let upstream = (record.phase == EnvironmentPhase::Active
                && component.runtime != Runtime::Static)
                .then(|| {
                    grant.as_ref().and_then(|grant| {
                        grant
                            .lease
                            .allocations
                            .iter()
                            .find(|a| Some(&a.key) == component.port_key.as_ref())
                            .and_then(|a| a.value.parse().ok())
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
                    spa_fallback: component.spa_fallback,
                    wake_timeout_seconds,
                });
            }
        }
    }
    routes.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(routes)
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
