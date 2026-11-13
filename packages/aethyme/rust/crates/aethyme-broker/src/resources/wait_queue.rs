//! Durable arrival order for host-resource waits.
//!
//! A request that meets ordinary contention while it is willing to wait
//! records one row in `resource_waiters`. Every later admission decision made
//! by a current binary respects those rows:
//!
//! - **Capacity pools:** a request may be granted only if the pool still has
//!   room for the units demanded by *every* live waiter that arrived before it,
//!   plus its own. A later, smaller request may therefore pass an earlier one
//!   only when that cannot take a unit the earlier one could use.
//! - **Exclusive keys:** a request may not take a key that an earlier live
//!   waiter demands.
//! - **Requests that do not wait** are treated as arriving after every live
//!   waiter, so they cannot take capacity or keys a waiter would need.
//!
//! Starvation freedom: the oldest live waiter has nobody ahead of it, so the
//! queue never gates it. No later grant may take a unit or key it demands
//! (every later request counts its demand as reserved), so the set of holders
//! it waits for only shrinks; holders are bounded by their lease lifecycle, so
//! it is granted in finite time, and the next waiter becomes the oldest.
//!
//! TCP ports and namespaces are not queued: a namespace never conflicts, and a
//! port's availability is decided by the operating system, which the store
//! cannot reserve for a waiter.
//!
//! Waiter rows are advisory to older binaries, which neither read nor write
//! them. The table is created additively with `CREATE TABLE IF NOT EXISTS` and
//! the host store version is unchanged, so older binaries keep opening it.

use super::*;

pub(super) const RESOURCE_WAITER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS resource_waiters (
 ticket INTEGER PRIMARY KEY,
 request_id TEXT NOT NULL UNIQUE,
 request_digest TEXT NOT NULL,
 repository TEXT NOT NULL,
 run_id TEXT NOT NULL,
 waiter_pid INTEGER,
 demands TEXT NOT NULL,
 enqueued_at INTEGER NOT NULL,
 deadline_at INTEGER NOT NULL,
 heartbeat_at INTEGER NOT NULL,
 times_skipped INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS resource_waiters_by_arrival
 ON resource_waiters(enqueued_at, ticket);
"#;

pub(super) const QUEUE_REPORT_SCHEMA: &str = "aethyme.host-resource-queue/v1";

/// A live waiter refreshes its heartbeat at least this often while it polls.
const WAITER_HEARTBEAT_REFRESH_MS: i64 = 30_000;
/// A waiter whose heartbeat is this old is treated as gone even when its PID
/// still answers (PID reuse) and its own deadline has not passed.
pub(super) const WAITER_STALE_MS: i64 = 120_000;

/// Sleep granularity between cheap change checks (`PRAGMA data_version`).
pub(super) const POLL_TICK: std::time::Duration = std::time::Duration::from_millis(250);
/// The head of a queue retries as soon as the store changes, and at least
/// this often without a change, to observe time-based events such as expiry.
pub(super) const HEAD_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(1);
/// Waiters behind the head retry at most this often on a store change.
pub(super) const TAIL_POLL_MIN: std::time::Duration = std::time::Duration::from_secs(1);
/// Upper bound of the backed-off retry interval behind the head.
pub(super) const TAIL_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// The queued part of a request: aggregated capacity per pool and exclusive
/// keys. Stored as JSON on the waiter row.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct WaiterDemand {
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub(super) struct QueuedWaiter {
    pub ticket: i64,
    pub request_id: String,
    pub request_digest: String,
    pub repository: String,
    pub run_id: String,
    pub waiter_pid: Option<i64>,
    /// `None` when the stored JSON could not be read; such a row is reaped.
    pub demands: Option<Vec<WaiterDemand>>,
    pub enqueued_at: i64,
    pub deadline_at: i64,
    pub heartbeat_at: i64,
    pub times_skipped: u64,
}

impl QueuedWaiter {
    fn arrival(&self) -> (i64, i64) {
        (self.enqueued_at, self.ticket)
    }

    /// Gone process, passed deadline, stale heartbeat or unreadable demands.
    pub fn is_dead(&self, now: i64) -> bool {
        self.demands.is_none()
            || self.deadline_at <= now
            || self.heartbeat_at.saturating_add(WAITER_STALE_MS) <= now
            || self.waiter_pid.is_some_and(holder_process_is_gone)
    }

    fn demands(&self) -> &[WaiterDemand] {
        self.demands.as_deref().unwrap_or(&[])
    }

    fn units_on(&self, pool: &str) -> u64 {
        self.demands()
            .iter()
            .filter(|demand| demand.kind == "capacity" && demand.value == pool)
            .filter_map(|demand| demand.units)
            .fold(0, u64::saturating_add)
    }

    fn wants_key(&self, name: &str) -> bool {
        self.demands()
            .iter()
            .any(|demand| demand.kind == "exclusive_key" && demand.value == name)
    }

    fn shares_any(&self, demands: &[WaiterDemand]) -> bool {
        demands.iter().any(|demand| self.shares(demand))
    }

    fn shares(&self, demand: &WaiterDemand) -> bool {
        self.demands()
            .iter()
            .any(|own| own.kind == demand.kind && own.value == demand.value)
    }
}

/// Registration data for a request that is willing to wait.
pub(super) struct WaitRegistration {
    pub pid: i64,
    pub deadline_at: i64,
    /// Original arrival time, kept if this waiter's row was reaped while it
    /// still polled (for example after the machine slept), so it re-enters at
    /// its original place instead of the back.
    pub enqueued_at: Option<i64>,
}

/// Queue standing of one request, reported by `acquire`/`run` contention
/// output and by `explain`. It never carries ownership credentials.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceQueueReport {
    pub schema: String,
    /// True when this request currently has a durable waiter record.
    pub queued: bool,
    /// Distinct live waiters ahead of this request on any queued resource it
    /// demands; 0 is the head. A request without a record counts every live
    /// waiter on its resources, which is where it would join.
    pub position: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at: Option<i64>,
    pub waited_ms: u64,
    /// Grants made to requests that arrived later while this one waited.
    pub times_skipped: u64,
    pub resources: Vec<HostResourceQueueStanding>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceQueueStanding {
    /// `capacity` or `exclusive_key`.
    pub kind: String,
    /// Pool or exclusive-key name.
    pub resource: String,
    pub waiters_ahead: u32,
    pub waiters_behind: u32,
    /// Capacity units demanded by the waiters ahead (0 for a key).
    pub units_ahead: u64,
    pub units_requested: u64,
    /// Units currently allocated to leases (0 or 1 holder for a key).
    pub units_allocated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    pub holders: Vec<HostResourceQueueHolder>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceQueueHolder {
    pub lease_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<u32>,
}

/// One live waiter as shown by inventory. No credentials.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceWaiter {
    pub request_id: String,
    pub repository: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiter_pid: Option<u32>,
    pub position: u32,
    pub enqueued_at: i64,
    pub deadline_at: i64,
    pub waited_ms: u64,
    pub times_skipped: u64,
    pub demands: Vec<HostResourceWaiterDemand>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceWaiterDemand {
    pub kind: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Phase-level evidence from one acquisition, for command metrics.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceWaitStats {
    pub waited_ms: u64,
    /// Store transactions that attempted the acquisition.
    pub attempts: u64,
    /// Cheap change checks between attempts.
    pub poll_ticks: u64,
    pub queued: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_at_enqueue: Option<u32>,
    pub times_skipped: u64,
    pub reaped_dead_waiters: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_queue: Option<HostResourceQueueReport>,
}

pub(super) fn waiters_table_exists(conn: &Connection) -> Result<bool, HostResourceError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_waiters')",
        [],
        |row| row.get(0),
    )?)
}

/// Every waiter row in arrival order. An absent table (a store no current
/// binary has opened for writing yet) is an empty queue.
pub(super) fn load_waiters(conn: &Connection) -> Result<Vec<QueuedWaiter>, HostResourceError> {
    if !waiters_table_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT ticket,request_id,request_digest,repository,run_id,waiter_pid,demands,\
         enqueued_at,deadline_at,heartbeat_at,times_skipped \
         FROM resource_waiters ORDER BY enqueued_at,ticket",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let demands: String = row.get(6)?;
            Ok(QueuedWaiter {
                ticket: row.get(0)?,
                request_id: row.get(1)?,
                request_digest: row.get(2)?,
                repository: row.get(3)?,
                run_id: row.get(4)?,
                waiter_pid: row.get(5)?,
                demands: serde_json::from_str(&demands).ok(),
                enqueued_at: row.get(7)?,
                deadline_at: row.get(8)?,
                heartbeat_at: row.get(9)?,
                times_skipped: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Live waiters only, without writing. Read-only callers use this.
pub(super) fn live_waiters(
    conn: &Connection,
    now: i64,
) -> Result<Vec<QueuedWaiter>, HostResourceError> {
    Ok(load_waiters(conn)?
        .into_iter()
        .filter(|waiter| !waiter.is_dead(now))
        .collect())
}

/// Delete dead waiter rows and return the live ones. Dead rows never gate
/// anyone, so reaping is only housekeeping; no operator action is needed.
pub(super) fn reap_dead_waiters(
    conn: &Connection,
    now: i64,
) -> Result<(Vec<QueuedWaiter>, u64), HostResourceError> {
    let mut live = Vec::new();
    let mut reaped = 0_u64;
    for waiter in load_waiters(conn)? {
        if waiter.is_dead(now) {
            reaped = reaped.saturating_add(conn.execute(
                "DELETE FROM resource_waiters WHERE ticket=?1 AND request_id=?2",
                params![waiter.ticket, waiter.request_id],
            )? as u64);
        } else {
            live.push(waiter);
        }
    }
    Ok((live, reaped))
}

pub(super) fn request_demands(request: &HostResourceRequest) -> Vec<WaiterDemand> {
    let mut capacity = BTreeMap::<&str, (u64, u32)>::new();
    let mut keys = BTreeSet::<&str>::new();
    for requirement in &request.resources {
        match &requirement.resource {
            HostResourceKind::Capacity { pool, units, limit } => {
                let entry = capacity.entry(pool.as_str()).or_insert((0, *limit));
                entry.0 = entry.0.saturating_add(u64::from(*units));
            }
            HostResourceKind::ExclusiveKey { name } => {
                keys.insert(name.as_str());
            }
            HostResourceKind::Namespace { .. } | HostResourceKind::TcpPort { .. } => {}
        }
    }
    capacity
        .into_iter()
        .map(|(pool, (units, limit))| WaiterDemand {
            kind: "capacity".into(),
            value: pool.into(),
            units: Some(units),
            limit: Some(limit),
        })
        .chain(keys.into_iter().map(|name| WaiterDemand {
            kind: "exclusive_key".into(),
            value: name.into(),
            units: None,
            limit: None,
        }))
        .collect()
}

/// This request's own live row, if any, and the live waiters ahead of it. A
/// request without a row is behind every live waiter.
pub(super) fn split_queue<'a>(
    live: &'a [QueuedWaiter],
    request_id: &str,
) -> (Option<&'a QueuedWaiter>, Vec<&'a QueuedWaiter>) {
    let own = live.iter().find(|waiter| waiter.request_id == request_id);
    let ahead = live
        .iter()
        .filter(|waiter| waiter.request_id != request_id)
        .filter(|waiter| own.is_none_or(|own| waiter.arrival() < own.arrival()))
        .collect();
    (own, ahead)
}

fn requirement_key_for<'a>(
    request: &'a HostResourceRequest,
    demand: &WaiterDemand,
) -> Option<&'a str> {
    request
        .resources
        .iter()
        .find(|requirement| match &requirement.resource {
            HostResourceKind::Capacity { pool, .. } => {
                demand.kind == "capacity" && *pool == demand.value
            }
            HostResourceKind::ExclusiveKey { name } => {
                demand.kind == "exclusive_key" && *name == demand.value
            }
            HostResourceKind::Namespace { .. } | HostResourceKind::TcpPort { .. } => false,
        })
        .map(|requirement| requirement.key.as_str())
}

fn allocated_units(occupied: &[Occupied], pool: &str) -> u64 {
    occupied
        .iter()
        .filter(|o| o.kind == "capacity" && o.value == pool)
        .filter_map(|o| o.units)
        .fold(0, |total, units| total.saturating_add(u64::from(units)))
}

/// Ordinary-contention conflicts that exist only because earlier waiters are
/// queued for the same resources. Resources the plan already refused are not
/// repeated.
pub(super) fn queue_conflicts(
    request: &HostResourceRequest,
    demands: &[WaiterDemand],
    occupied: &[Occupied],
    ahead: &[&QueuedWaiter],
    existing: &[HostResourceConflict],
) -> Vec<HostResourceConflict> {
    let mut conflicts = Vec::new();
    for demand in demands {
        let Some(key) = requirement_key_for(request, demand) else {
            continue;
        };
        if existing.iter().any(|conflict| conflict.resource_key == key) {
            continue;
        }
        let earlier = ahead
            .iter()
            .filter(|waiter| waiter.shares(demand))
            .collect::<Vec<_>>();
        if earlier.is_empty() {
            continue;
        }
        if demand.kind == "capacity" {
            let pool = demand.value.as_str();
            let limit = u64::from(demand.limit.unwrap_or(0));
            let requested = demand.units.unwrap_or(0);
            let used = allocated_units(occupied, pool);
            let reserved = earlier
                .iter()
                .map(|waiter| waiter.units_on(pool))
                .fold(0, u64::saturating_add);
            if used.saturating_add(reserved).saturating_add(requested) > limit {
                conflicts.push(conflict(
                    "resource_contention",
                    key,
                    "capacity",
                    format!("{pool}:{requested}/{limit}"),
                    &format!(
                        "pool has {used}/{limit} units allocated and {reserved} unit(s) reserved for {} earlier waiting request(s); this bundle requests {requested} units and waits its turn",
                        earlier.len()
                    ),
                    occupied
                        .iter()
                        .find(|o| o.kind == "capacity" && o.value == pool)
                        .map(|o| o.lease_id.clone()),
                ));
            }
        } else {
            conflicts.push(conflict(
                "resource_contention",
                key,
                "exclusive_key",
                demand.value.clone(),
                &format!(
                    "exclusive key is demanded by {} earlier waiting request(s); this bundle waits its turn",
                    earlier.len()
                ),
                None,
            ));
        }
    }
    conflicts
}

pub(super) fn queue_report(
    demands: &[WaiterDemand],
    occupied: &[Occupied],
    live: &[QueuedWaiter],
    own: Option<&QueuedWaiter>,
    ahead: &[&QueuedWaiter],
    now: i64,
) -> HostResourceQueueReport {
    let behind = |demand: &WaiterDemand| {
        own.map_or(0, |own| {
            live.iter()
                .filter(|waiter| waiter.request_id != own.request_id)
                .filter(|waiter| waiter.arrival() > own.arrival() && waiter.shares(demand))
                .count()
        })
    };
    let resources = demands
        .iter()
        .map(|demand| {
            let earlier = ahead
                .iter()
                .filter(|waiter| waiter.shares(demand))
                .collect::<Vec<_>>();
            let holders = occupied
                .iter()
                .filter(|o| o.kind == demand.kind && o.value == demand.value)
                .map(|o| HostResourceQueueHolder {
                    lease_id: o.lease_id.clone(),
                    generation: u64::try_from(o.generation).unwrap_or(0),
                    units: o.units,
                    holder_pid: o.holder_pid.and_then(|pid| u32::try_from(pid).ok()),
                })
                .collect::<Vec<_>>();
            HostResourceQueueStanding {
                kind: demand.kind.clone(),
                resource: demand.value.clone(),
                waiters_ahead: u32::try_from(earlier.len()).unwrap_or(u32::MAX),
                waiters_behind: u32::try_from(behind(demand)).unwrap_or(u32::MAX),
                units_ahead: if demand.kind == "capacity" {
                    earlier
                        .iter()
                        .map(|waiter| waiter.units_on(&demand.value))
                        .fold(0, u64::saturating_add)
                } else {
                    0
                },
                units_requested: demand.units.unwrap_or(1),
                units_allocated: if demand.kind == "capacity" {
                    allocated_units(occupied, &demand.value)
                } else {
                    holders.len() as u64
                },
                limit: demand.limit,
                holders,
            }
        })
        .collect();
    let position = ahead
        .iter()
        .filter(|waiter| waiter.shares_any(demands))
        .count();
    HostResourceQueueReport {
        schema: QUEUE_REPORT_SCHEMA.into(),
        queued: own.is_some(),
        position: u32::try_from(position).unwrap_or(u32::MAX),
        enqueued_at: own.map(|own| own.enqueued_at),
        waited_ms: own.map_or(0, |own| {
            u64::try_from(now.saturating_sub(own.enqueued_at)).unwrap_or(0)
        }),
        times_skipped: own.map_or(0, |own| own.times_skipped),
        resources,
    }
}

/// Insert this request's waiter row, or refresh its heartbeat. Returns
/// whether it wrote, so an otherwise unchanged attempt can roll back and not
/// wake every other waiter.
pub(super) fn register_waiter(
    conn: &Connection,
    request: &HostResourceRequest,
    digest: &str,
    demands: &[WaiterDemand],
    registration: &WaitRegistration,
    own: Option<&QueuedWaiter>,
    now: i64,
) -> Result<bool, HostResourceError> {
    match own {
        Some(own) => {
            let stale = now.saturating_sub(own.heartbeat_at) >= WAITER_HEARTBEAT_REFRESH_MS;
            if !stale
                && own.waiter_pid == Some(registration.pid)
                && own.deadline_at == registration.deadline_at
            {
                return Ok(false);
            }
            conn.execute(
                "UPDATE resource_waiters SET heartbeat_at=?2,waiter_pid=?3,deadline_at=?4 \
                 WHERE ticket=?1",
                params![own.ticket, now, registration.pid, registration.deadline_at],
            )?;
            Ok(true)
        }
        None => {
            let demands = serde_json::to_string(demands)
                .map_err(|error| HostResourceError::InvalidRequest(error.to_string()))?;
            conn.execute(
                "INSERT INTO resource_waiters \
                 (request_id,request_digest,repository,run_id,waiter_pid,demands,\
                  enqueued_at,deadline_at,heartbeat_at,times_skipped) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0)",
                params![
                    request.request_id,
                    digest,
                    request.repository,
                    request.run_id,
                    registration.pid,
                    demands,
                    registration.enqueued_at.unwrap_or(now).min(now),
                    registration.deadline_at,
                    now
                ],
            )?;
            Ok(true)
        }
    }
}

/// Count one skip on every earlier waiter this grant passed.
pub(super) fn record_skips(
    conn: &Connection,
    ahead: &[&QueuedWaiter],
    demands: &[WaiterDemand],
) -> Result<(), HostResourceError> {
    for waiter in ahead.iter().filter(|waiter| waiter.shares_any(demands)) {
        conn.execute(
            "UPDATE resource_waiters SET times_skipped=times_skipped+1 WHERE ticket=?1",
            [waiter.ticket],
        )?;
    }
    Ok(())
}

pub(super) fn delete_waiter(
    conn: &Connection,
    request_id: &str,
    pid: Option<i64>,
) -> Result<usize, HostResourceError> {
    if !waiters_table_exists(conn)? {
        return Ok(0);
    }
    Ok(match pid {
        Some(pid) => conn.execute(
            "DELETE FROM resource_waiters WHERE request_id=?1 AND waiter_pid=?2",
            params![request_id, pid],
        )?,
        None => conn.execute(
            "DELETE FROM resource_waiters WHERE request_id=?1",
            [request_id],
        )?,
    })
}

/// Public inventory of live waiters, each with its position in its own
/// resources' queues.
pub(super) fn waiter_inventory(live: &[QueuedWaiter], now: i64) -> Vec<HostResourceWaiter> {
    live.iter()
        .map(|waiter| {
            let position = live
                .iter()
                .filter(|other| other.request_id != waiter.request_id)
                .filter(|other| other.arrival() < waiter.arrival())
                .filter(|other| other.shares_any(waiter.demands()))
                .count();
            HostResourceWaiter {
                request_id: waiter.request_id.clone(),
                repository: waiter.repository.clone(),
                run_id: waiter.run_id.clone(),
                waiter_pid: waiter.waiter_pid.and_then(|pid| u32::try_from(pid).ok()),
                position: u32::try_from(position).unwrap_or(u32::MAX),
                enqueued_at: waiter.enqueued_at,
                deadline_at: waiter.deadline_at,
                waited_ms: u64::try_from(now.saturating_sub(waiter.enqueued_at)).unwrap_or(0),
                times_skipped: waiter.times_skipped,
                demands: waiter
                    .demands()
                    .iter()
                    .map(|demand| HostResourceWaiterDemand {
                        kind: demand.kind.clone(),
                        resource: demand.value.clone(),
                        units: demand.units,
                        limit: demand.limit,
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Human summary of a queue standing for progress messages.
pub(crate) fn describe_queue(queue: &HostResourceQueueReport) -> String {
    let units_ahead = queue
        .resources
        .iter()
        .map(|standing| standing.units_ahead)
        .fold(0, u64::saturating_add);
    format!(
        "queue position {} ({} unit(s) ahead), waited {}ms, skipped {} time(s)",
        queue.position, units_ahead, queue.waited_ms, queue.times_skipped
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, resource: HostResourceKind) -> HostResourceRequest {
        HostResourceRequest {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: id.into(),
            repository: "owner/repo".into(),
            worktree_fingerprint: "fingerprint".into(),
            run_id: format!("run-{id}"),
            ttl_seconds: 60,
            holder_pid: Some(std::process::id()),
            resources: vec![HostResourceRequirement {
                key: "slot".into(),
                resource,
            }],
        }
    }

    fn capacity(id: &str, units: u32) -> HostResourceRequest {
        request(
            id,
            HostResourceKind::Capacity {
                pool: "pool".into(),
                units,
                limit: 4,
            },
        )
    }

    fn key(id: &str, name: &str) -> HostResourceRequest {
        request(id, HostResourceKind::ExclusiveKey { name: name.into() })
    }

    fn registration() -> WaitRegistration {
        WaitRegistration {
            pid: i64::from(std::process::id()),
            deadline_at: now_ms() + 600_000,
            enqueued_at: None,
        }
    }

    /// One waiting attempt that must be refused; returns its standing.
    fn contended(
        coordinator: &mut HostResourceCoordinator,
        request: &HostResourceRequest,
    ) -> HostResourceQueueReport {
        let attempt = coordinator
            .acquire_attempt(request, false, Some(&registration()))
            .unwrap();
        let Err(conflict) = attempt.result else {
            panic!("{} must wait", request.request_id);
        };
        assert_eq!(conflict.code, "resource_contention");
        attempt.queue
    }

    fn granted(
        coordinator: &mut HostResourceCoordinator,
        request: &HostResourceRequest,
    ) -> HostResourceGrant {
        match coordinator
            .acquire_attempt(request, false, Some(&registration()))
            .unwrap()
            .result
        {
            Ok(grant) => grant,
            Err(conflict) => panic!(
                "{} must be granted: {}",
                request.request_id, conflict.message
            ),
        }
    }

    fn release(coordinator: &mut HostResourceCoordinator, grant: &HostResourceGrant) {
        coordinator
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
    }

    fn waiter_rows(coordinator: &HostResourceCoordinator) -> Vec<String> {
        load_waiters(&coordinator.conn)
            .unwrap()
            .into_iter()
            .map(|waiter| waiter.request_id)
            .collect()
    }

    fn conflict_reason(error: HostResourceError) -> String {
        match error {
            HostResourceError::Conflict {
                code, conflicts, ..
            } => {
                assert_eq!(code, "resource_contention");
                conflicts
                    .into_iter()
                    .map(|conflict| conflict.reason)
                    .collect::<Vec<_>>()
                    .join("; ")
            }
            other => panic!("expected contention, got {other:?}"),
        }
    }

    fn reaped_pid() -> i64 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        i64::from(pid)
    }

    #[test]
    fn full_pool_request_that_arrived_first_is_granted_before_later_smaller_ones() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let holder = c.acquire(&capacity("holder", 2)).unwrap();
        let big = capacity("big", 4);
        let small = capacity("small", 2);

        let queue = contended(&mut c, &big);
        assert!(queue.queued);
        assert_eq!(queue.position, 0);
        // Two units are free, but taking them would delay the full-pool waiter.
        let reason = conflict_reason(c.acquire(&capacity("small-now", 2)).unwrap_err());
        assert!(
            reason.contains("reserved for 1 earlier waiting"),
            "{reason}"
        );
        let queue = contended(&mut c, &small);
        assert_eq!(queue.position, 1);
        assert_eq!(queue.resources[0].units_ahead, 4);

        release(&mut c, &holder);
        // The empty pool belongs to the earlier full-pool request.
        assert!(
            c.acquire_attempt(&small, false, Some(&registration()))
                .unwrap()
                .result
                .is_err()
        );
        let big_grant = granted(&mut c, &big);
        assert_eq!(waiter_rows(&c), vec!["small".to_string()]);
        assert_eq!(
            load_waiters(&c.conn).unwrap()[0].times_skipped,
            0,
            "nothing passed the smaller waiter"
        );
        release(&mut c, &big_grant);
        granted(&mut c, &small);
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn dead_waiters_are_skipped_and_reaped() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let now = now_ms();
        let demands = r#"[{"kind":"capacity","value":"pool","units":4,"limit":4}]"#;
        for (request_id, pid, heartbeat_at) in [
            // The waiting process is gone.
            ("gone", reaped_pid(), now),
            // The PID answers (it is this test), but the waiter stopped polling.
            (
                "stale",
                i64::from(std::process::id()),
                now - WAITER_STALE_MS - 1,
            ),
        ] {
            c.conn
                .execute(
                    "INSERT INTO resource_waiters (request_id,request_digest,repository,run_id,\
                     waiter_pid,demands,enqueued_at,deadline_at,heartbeat_at) \
                     VALUES (?1,'digest','owner/repo','run',?2,?3,?4,?5,?6)",
                    params![
                        request_id,
                        pid,
                        demands,
                        now - 1_000,
                        now + 600_000,
                        heartbeat_at
                    ],
                )
                .unwrap();
        }
        let next = capacity("next", 2);
        let explained = c.explain(&next).unwrap();
        assert!(explained.available, "dead waiters must not gate anyone");
        assert_eq!(explained.queue.unwrap().position, 0);
        assert_eq!(waiter_rows(&c).len(), 2, "explain is read-only");

        let attempt = c.acquire_attempt(&next, false, None).unwrap();
        assert!(attempt.result.is_ok());
        assert_eq!(attempt.reaped, 2);
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn timed_out_waiter_leaves_no_record() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        c.acquire(&key("holder", "shared")).unwrap();
        let error = c
            .acquire_with_wait(
                &key("waiter", "shared"),
                std::time::Duration::from_millis(300),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, HostResourceError::Conflict { .. }));
        assert!(waiter_rows(&c).is_empty());
        let stats = c.last_wait_stats().unwrap();
        assert!(stats.queued);
        assert_eq!(stats.position_at_enqueue, Some(0));
        assert!(stats.attempts >= 2, "{stats:?}");
        assert!(stats.final_queue.as_ref().unwrap().queued);
    }

    #[test]
    fn non_waiting_request_does_not_jump_the_queue() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let holder = c.acquire(&key("holder", "shared")).unwrap();
        let first = key("first", "shared");
        contended(&mut c, &first);
        release(&mut c, &holder);

        let reason = conflict_reason(c.acquire(&key("jumper", "shared")).unwrap_err());
        assert!(reason.contains("earlier waiting request"), "{reason}");
        // A key nobody waits for is unaffected.
        c.acquire(&key("unrelated", "other")).unwrap();
        granted(&mut c, &first);
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn queue_position_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let holder = c.acquire(&capacity("holder", 4)).unwrap();
        contended(&mut c, &capacity("first", 2));
        contended(&mut c, &capacity("second", 2));
        let third = capacity("third", 1);
        let queue = contended(&mut c, &third);
        assert_eq!(queue.position, 2);

        let explained = c.explain(&third).unwrap().queue.unwrap();
        assert!(explained.queued);
        assert_eq!(explained.position, 2);
        let standing = &explained.resources[0];
        assert_eq!(standing.resource, "pool");
        assert_eq!(standing.waiters_ahead, 2);
        assert_eq!(standing.waiters_behind, 0);
        assert_eq!(standing.units_ahead, 4);
        assert_eq!(standing.units_allocated, 4);
        assert_eq!(standing.holders[0].lease_id, holder.lease.lease_id);

        let fresh = c.explain(&capacity("fresh", 1)).unwrap().queue.unwrap();
        assert!(!fresh.queued);
        assert_eq!(fresh.position, 3, "a new request would join behind three");

        let positions = c
            .waiting()
            .unwrap()
            .into_iter()
            .map(|waiter| (waiter.request_id, waiter.position))
            .collect::<Vec<_>>();
        assert_eq!(
            positions,
            vec![
                ("first".to_string(), 0),
                ("second".to_string(), 1),
                ("third".to_string(), 2)
            ]
        );
    }

    #[test]
    fn old_style_acquire_against_migrated_store_still_works() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("h.db");
        // A store as the previous release leaves it: host store v2, no queue.
        {
            let previous = Connection::open(&db).unwrap();
            previous.execute_batch(SCHEMA).unwrap();
            previous.execute_batch(SUPERVISED_HOLD_MIGRATION).unwrap();
            previous
                .execute("UPDATE meta SET value=2 WHERE key='schema_version'", [])
                .unwrap();
        }
        let reader = HostResourceCoordinator::open_read_only(&db).unwrap();
        assert!(!waiters_table_exists(&reader.conn).unwrap());
        let explained = reader.explain(&capacity("reader", 1)).unwrap();
        assert!(explained.available);
        assert_eq!(explained.queue.unwrap().position, 0);
        drop(reader);

        let mut c = HostResourceCoordinator::open(&db).unwrap();
        assert!(waiters_table_exists(&c.conn).unwrap());
        assert_eq!(
            store_schema_version(&c.conn).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION,
            "the queue is additive; the store version is unchanged"
        );
        let holder = c.acquire(&capacity("holder", 4)).unwrap();
        contended(&mut c, &capacity("waiter", 2));
        release(&mut c, &holder);

        // An older binary's open and acquire: its schema statements, its
        // version check, and an insert that knows nothing about waiters.
        let mut old = Connection::open(&db).unwrap();
        old.execute_batch(SCHEMA).unwrap();
        assert_eq!(
            validate_schema(&old).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION
        );
        let tx = old
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let generation: i64 = tx
            .query_row(
                "UPDATE meta SET value=value+1 WHERE key='generation' RETURNING value",
                [],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute(
            "INSERT INTO resource_leases (lease_id,request_id,request_digest,repository,\
             worktree_fingerprint,run_id,generation,ownership_token,state,holder_pid,\
             created_at,expires_at,updated_at) \
             VALUES ('old-lease','old-request','old-digest','owner/repo','fingerprint',\
                     'old-run',?1,'old-token','active',?2,?3,?4,?3)",
            params![
                generation,
                i64::from(std::process::id()),
                now_ms(),
                now_ms() + 60_000
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO resource_allocations VALUES ('old-lease','slot','capacity','pool',2,4)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        drop(old);

        // The older binary took two units ahead of the queue (it cannot see
        // it), but the queue itself is intact and still ordered.
        assert_eq!(waiter_rows(&c), vec!["waiter".to_string()]);
        let queue = c.explain(&capacity("waiter", 2)).unwrap().queue.unwrap();
        assert!(queue.queued);
        assert_eq!(queue.resources[0].units_allocated, 2);
        granted(&mut c, &capacity("waiter", 2));
        assert!(waiter_rows(&c).is_empty());
    }
}
