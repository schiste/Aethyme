//! Durable arrival order for host-resource waits.
//!
//! A request that meets ordinary contention while it is willing to wait
//! records one row in `resource_waiters`. Every later admission decision made
//! by a current binary respects those rows:
//!
//! **What a waiter reserves.** A waiter reserves a pool or key only once that
//! resource has itself been contended for it (the pool did not fit, the key
//! was held, or an earlier waiter's reservation was in the way). Resources in
//! its bundle that were free stay usable by others. A reservation is sticky
//! for the rest of the wait, so a resource cannot slip back and forth between
//! reserved and taken. If any current conflict is on something the queue does
//! not manage (a TCP port), the waiter reserves nothing for that attempt: it
//! keeps its place in line but holds no pool or key against anyone while it
//! could not run anyway.
//!
//! **Admission.** A request may be granted only if, for every pool it demands,
//! the pool has room for what is allocated plus the units reserved by every
//! earlier live waiter, plus its own (against the smallest limit any of them
//! declared), and it takes no key an earlier live waiter has reserved.
//! Requests that do not wait are ordered after every live waiter.
//!
//! **Starvation freedom** (for bundles of pools and keys). The oldest live
//! waiter W has nobody ahead, so the queue never gates it. While W is not
//! granted, some resource of its bundle is contended and becomes reserved;
//! reservations only accumulate, so after at most one event per resource in
//! its bundle every resource that blocks W is reserved. From then on no later
//! grant can take a reserved unit or key: a reserved pool's allocation only
//! falls until W fits and then keeps room for W, and a reserved key is not
//! granted to anyone else. Holders end in finite time (release, TTL expiry, or
//! reclaim of a dead holder), so W is granted; the next waiter becomes the
//! oldest, and by induction every waiter is served. A bundle that also needs a
//! TCP port is outside this guarantee while the port is short, because the
//! operating system, not the store, decides when a port frees.
//!
//! **Order** is the row's `AUTOINCREMENT` ticket alone, so a wall-clock step
//! cannot reorder the queue; a waiter whose row was reaped while it still
//! polled re-enters with its original ticket.
//!
//! Waiter rows are advisory to older binaries, which neither read nor write
//! them. The table is created additively with `CREATE TABLE IF NOT EXISTS` and
//! the host store version is unchanged, so older binaries keep opening it.

use super::*;

pub(super) const RESOURCE_WAITER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS resource_waiters (
 ticket INTEGER PRIMARY KEY AUTOINCREMENT,
 request_id TEXT NOT NULL UNIQUE,
 request_digest TEXT NOT NULL,
 repository TEXT NOT NULL,
 run_id TEXT NOT NULL,
 waiter_pid INTEGER,
 demands TEXT NOT NULL,
 reserved TEXT NOT NULL DEFAULT '[]',
 enqueued_at INTEGER NOT NULL,
 deadline_at INTEGER NOT NULL,
 heartbeat_at INTEGER NOT NULL,
 times_skipped INTEGER NOT NULL DEFAULT 0
);
"#;

pub(super) const QUEUE_REPORT_SCHEMA: &str = "aethyme.host-resource-queue/v1";

/// A live waiter refreshes its heartbeat at least this often while it polls.
const WAITER_HEARTBEAT_REFRESH_MS: i64 = 30_000;
/// A waiter whose heartbeat is this old is treated as gone even when its PID
/// still answers (PID reuse) and its own deadline has not passed.
pub(super) const WAITER_STALE_MS: i64 = 120_000;
/// A row past its own deadline by more than this is dead.
const WAITER_DEADLINE_GRACE_MS: i64 = 2_000;

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
    /// Everything the request needs from the queue. `None` when the stored
    /// JSON could not be read; such a row is reaped.
    pub demands: Option<Vec<WaiterDemand>>,
    /// The subset it currently holds against later requests. `None` when
    /// unreadable; such a row is reaped.
    pub reserved: Option<Vec<WaiterDemand>>,
    pub enqueued_at: i64,
    pub deadline_at: i64,
    pub heartbeat_at: i64,
    pub times_skipped: u64,
}

impl QueuedWaiter {
    fn arrival(&self) -> i64 {
        self.ticket
    }

    /// Gone process, passed deadline, stale heartbeat or unreadable demands.
    /// The short deadline grace lets a waiter's own final attempt, made at
    /// its deadline, still see its row instead of reaping it.
    pub fn is_dead(&self, now: i64) -> bool {
        self.demands.is_none()
            || self.reserved.is_none()
            || self.deadline_at.saturating_add(WAITER_DEADLINE_GRACE_MS) <= now
            || self.heartbeat_at.saturating_add(WAITER_STALE_MS) <= now
            || self.waiter_pid.is_some_and(holder_process_is_gone)
    }

    fn demands(&self) -> &[WaiterDemand] {
        self.demands.as_deref().unwrap_or(&[])
    }

    pub fn reserved(&self) -> &[WaiterDemand] {
        self.reserved.as_deref().unwrap_or(&[])
    }

    fn units_on(&self, pool: &str) -> u64 {
        self.reserved()
            .iter()
            .filter(|demand| demand.kind == "capacity" && demand.value == pool)
            .filter_map(|demand| demand.units)
            .fold(0, u64::saturating_add)
    }

    fn shares_any(&self, demands: &[WaiterDemand]) -> bool {
        demands.iter().any(|demand| self.shares(demand))
    }

    /// Whether this waiter currently reserves the same pool or key.
    fn shares(&self, demand: &WaiterDemand) -> bool {
        self.reserved()
            .iter()
            .any(|own| own.kind == demand.kind && own.value == demand.value)
    }

    /// Smallest limit this waiter reserved the pool under.
    fn limit_on(&self, pool: &str) -> Option<u32> {
        self.reserved()
            .iter()
            .filter(|demand| demand.kind == "capacity" && demand.value == pool)
            .filter_map(|demand| demand.limit)
            .min()
    }
}

/// Registration data for a request that is willing to wait.
pub(super) struct WaitRegistration {
    pub pid: i64,
    pub deadline_at: i64,
    /// Original ticket and arrival time, kept if this waiter's row was reaped
    /// while it still polled (for example after a long stall), so it re-enters
    /// at its original place instead of the back.
    pub ticket: Option<i64>,
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
    /// Whether the waiter currently holds this resource against later
    /// requests (it has been contended for it).
    pub reserved: bool,
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
         enqueued_at,deadline_at,heartbeat_at,times_skipped,reserved \
         FROM resource_waiters ORDER BY ticket",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let demands: String = row.get(6)?;
            let reserved: String = row.get(11)?;
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
                reserved: serde_json::from_str(&reserved).ok(),
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
            // Against the smallest declared limit: a request with a larger
            // limit must not overbook a pool earlier waiters reserved.
            let limit = u64::from(
                earlier
                    .iter()
                    .filter_map(|waiter| waiter.limit_on(pool))
                    .chain(demand.limit)
                    .min()
                    .unwrap_or(0),
            );
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

/// Whether registering would write: no row yet, a heartbeat due, or a changed
/// process or deadline. An attempt that would not write commits nothing, so
/// it does not wake every other waiter.
pub(super) fn registration_needs_write(
    own: Option<&QueuedWaiter>,
    reserved: &[WaiterDemand],
    registration: &WaitRegistration,
    now: i64,
) -> bool {
    own.is_none_or(|own| {
        now.saturating_sub(own.heartbeat_at) >= WAITER_HEARTBEAT_REFRESH_MS
            || own.waiter_pid != Some(registration.pid)
            || own.deadline_at != registration.deadline_at
            || own.reserved() != reserved
    })
}

/// Contended resources of this request: the pools and keys named by an
/// ordinary-contention conflict (holders or earlier reservations in the way).
/// `None` when a conflict is on something the queue does not manage, such as
/// a TCP port: then the waiter reserves nothing for this attempt.
pub(super) fn contended_demands(
    request: &HostResourceRequest,
    demands: &[WaiterDemand],
    conflicts: &[HostResourceConflict],
) -> Option<Vec<WaiterDemand>> {
    if conflicts
        .iter()
        .any(|conflict| conflict.kind != "capacity" && conflict.kind != "exclusive_key")
    {
        return None;
    }
    Some(
        demands
            .iter()
            .filter(|demand| {
                requirement_key_for(request, demand).is_some_and(|key| {
                    conflicts.iter().any(|conflict| {
                        conflict.code == "resource_contention" && conflict.resource_key == key
                    })
                })
            })
            .cloned()
            .collect(),
    )
}

/// What this waiter reserves after an attempt: its earlier reservations plus
/// the resources contended now (sticky), or nothing while an unqueued
/// resource blocks it. Kept in the order of `demands`.
pub(super) fn next_reservation(
    demands: &[WaiterDemand],
    contended: Option<&[WaiterDemand]>,
    own: Option<&QueuedWaiter>,
) -> Vec<WaiterDemand> {
    let Some(contended) = contended else {
        return Vec::new();
    };
    let same = |a: &WaiterDemand, b: &WaiterDemand| a.kind == b.kind && a.value == b.value;
    demands
        .iter()
        .filter(|demand| {
            contended.iter().any(|other| same(demand, other))
                || own.is_some_and(|own| own.reserved().iter().any(|other| same(demand, other)))
        })
        .cloned()
        .collect()
}

/// Insert this request's waiter row, or refresh its heartbeat. Returns
/// whether it wrote.
#[allow(
    clippy::too_many_arguments,
    reason = "the row binds request identity, demand sets and registration explicitly"
)]
pub(super) fn register_waiter(
    conn: &Connection,
    request: &HostResourceRequest,
    digest: &str,
    demands: &[WaiterDemand],
    reserved: &[WaiterDemand],
    registration: &WaitRegistration,
    own: Option<&QueuedWaiter>,
    now: i64,
) -> Result<bool, HostResourceError> {
    let to_json = |value: &[WaiterDemand]| {
        serde_json::to_string(value)
            .map_err(|error| HostResourceError::InvalidRequest(error.to_string()))
    };
    match own {
        Some(own) => {
            if !registration_needs_write(Some(own), reserved, registration, now) {
                return Ok(false);
            }
            conn.execute(
                "UPDATE resource_waiters SET heartbeat_at=?2,waiter_pid=?3,deadline_at=?4,\
                 reserved=?5 WHERE ticket=?1",
                params![
                    own.ticket,
                    now,
                    registration.pid,
                    registration.deadline_at,
                    to_json(reserved)?
                ],
            )?;
            Ok(true)
        }
        None => {
            // A re-entering waiter keeps its original ticket. AUTOINCREMENT
            // never hands a deleted ticket to anyone else, so it is free.
            conn.execute(
                "INSERT INTO resource_waiters \
                 (ticket,request_id,request_digest,repository,run_id,waiter_pid,demands,\
                  reserved,enqueued_at,deadline_at,heartbeat_at,times_skipped) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0)",
                params![
                    registration.ticket,
                    request.request_id,
                    digest,
                    request.repository,
                    request.run_id,
                    registration.pid,
                    to_json(demands)?,
                    to_json(reserved)?,
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
                        reserved: waiter.shares(demand),
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
            ticket: None,
            enqueued_at: None,
        }
    }

    /// One waiting attempt that must be refused; returns its standing.
    fn contended(
        coordinator: &mut HostResourceCoordinator,
        request: &HostResourceRequest,
    ) -> HostResourceQueueReport {
        let attempt = coordinator
            .acquire_attempt(request, false, Some(&registration()), now_ms())
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
            .acquire_attempt(request, false, Some(&registration()), now_ms())
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
            c.acquire_attempt(&small, false, Some(&registration()), now_ms())
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
                     waiter_pid,demands,reserved,enqueued_at,deadline_at,heartbeat_at) \
                     VALUES (?1,'digest','owner/repo','run',?2,?3,?3,?4,?5,?6)",
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

        let attempt = c.acquire_attempt(&next, false, None, now_ms()).unwrap();
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

    fn pool(key: &str, units: u32, limit: u32) -> HostResourceRequirement {
        HostResourceRequirement {
            key: key.into(),
            resource: HostResourceKind::Capacity {
                pool: "pool".into(),
                units,
                limit,
            },
        }
    }

    fn bundle(id: &str, resources: Vec<HostResourceRequirement>) -> HostResourceRequest {
        let mut request = capacity(id, 1);
        request.resources = resources;
        request
    }

    fn reserved_of(coordinator: &HostResourceCoordinator, request_id: &str) -> Vec<String> {
        load_waiters(&coordinator.conn)
            .unwrap()
            .into_iter()
            .find(|waiter| waiter.request_id == request_id)
            .unwrap()
            .reserved()
            .iter()
            .map(|demand| format!("{}:{}", demand.kind, demand.value))
            .collect()
    }

    #[test]
    fn a_waiter_blocked_on_a_port_reserves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let listener =
            std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        c.acquire(&capacity("holder", 2)).unwrap();
        let waiter = bundle(
            "port-waiter",
            vec![
                pool("slot", 4, 4),
                HostResourceRequirement {
                    key: "port".into(),
                    resource: HostResourceKind::TcpPort {
                        start: port,
                        end: port,
                    },
                },
            ],
        );
        let queue = contended(&mut c, &waiter);
        assert!(queue.queued, "it keeps its place in line");
        assert!(reserved_of(&c, "port-waiter").is_empty());
        // The two free units are not held for a request that cannot run.
        c.acquire(&capacity("later", 2)).unwrap();
        drop(listener);
    }

    #[test]
    fn a_waiter_reserves_only_what_was_contended_for_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let key_holder = c.acquire(&key("key-holder", "shared")).unwrap();
        let waiter = bundle(
            "bundle",
            vec![
                pool("slot", 2, 4),
                HostResourceRequirement {
                    key: "lock".into(),
                    resource: HostResourceKind::ExclusiveKey {
                        name: "shared".into(),
                    },
                },
            ],
        );
        contended(&mut c, &waiter);
        assert_eq!(reserved_of(&c, "bundle"), vec!["exclusive_key:shared"]);
        // The pool was free for the waiter, so it is not held against others.
        let whole = c.acquire(&capacity("whole", 4)).unwrap();

        release(&mut c, &key_holder);
        // The key it waited for is free but still its turn.
        let reason = conflict_reason(c.acquire(&key("jumper", "shared")).unwrap_err());
        assert!(reason.contains("earlier waiting request"), "{reason}");
        // Now the pool blocks it: the pool joins its (sticky) reservation.
        contended(&mut c, &waiter);
        assert_eq!(
            reserved_of(&c, "bundle"),
            vec!["capacity:pool", "exclusive_key:shared"]
        );
        release(&mut c, &whole);
        let reason = conflict_reason(c.acquire(&capacity("three", 3)).unwrap_err());
        assert!(reason.contains("reserved for 1 earlier"), "{reason}");
        c.acquire(&capacity("one", 1)).unwrap();
        granted(&mut c, &waiter);
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn a_larger_declared_limit_cannot_overbook_an_earlier_reservation() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let holder = c.acquire(&capacity("holder", 4)).unwrap();
        let first = capacity("first", 4);
        contended(&mut c, &first);
        release(&mut c, &holder);

        let larger = bundle("larger", vec![pool("slot", 2, 8)]);
        let reason = conflict_reason(c.acquire(&larger).unwrap_err());
        assert!(reason.contains("reserved for 1 earlier"), "{reason}");
        granted(&mut c, &first);
        // Against the live holder the different limit is a policy refusal,
        // which waiting cannot resolve, so it leaves no waiter row.
        let attempt = c
            .acquire_attempt(&larger, false, Some(&registration()), now_ms())
            .unwrap();
        let Err(conflict) = attempt.result else {
            panic!("a mismatched limit must be refused");
        };
        assert_eq!(conflict.code, "capacity_policy_mismatch");
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn a_wait_longer_than_the_stale_threshold_keeps_its_place() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        let mut holder = capacity("holder", 4);
        holder.ttl_seconds = 3_600;
        c.acquire(&holder).unwrap();
        let base = now_ms();
        let registration = WaitRegistration {
            pid: i64::from(std::process::id()),
            deadline_at: base + 36_000_000,
            ticket: None,
            enqueued_at: None,
        };
        let first = capacity("first", 2);
        let attempt = c
            .acquire_attempt(&first, false, Some(&registration), base)
            .unwrap();
        let ticket = attempt.ticket.unwrap();
        // Keep polling for well over the stale threshold.
        for step in 1..=5 {
            let now = base + step * (WAITER_STALE_MS / 2);
            let attempt = c
                .acquire_attempt(&first, false, Some(&registration), now)
                .unwrap();
            assert!(attempt.result.is_err());
            assert_eq!(attempt.reaped, 0, "the waiter's own row must stay live");
            assert_eq!(attempt.ticket, Some(ticket), "it must keep its place");
        }
        let later = capacity("later", 2);
        let attempt = c
            .acquire_attempt(
                &later,
                false,
                Some(&registration),
                base + 3 * WAITER_STALE_MS,
            )
            .unwrap();
        assert_eq!(attempt.queue.position, 1);
    }

    #[test]
    fn a_wait_whose_wall_clock_deadline_passed_ends_without_a_dead_row() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        c.acquire(&key("holder", "shared")).unwrap();
        let base = now_ms();
        let calls = std::cell::Cell::new(0_u32);
        // The process "sleeps" for an hour right after its first attempt.
        let mut clock = || {
            let call = calls.get();
            calls.set(call + 1);
            if call < 2 { base } else { base + 3_600_000 }
        };
        let started = std::time::Instant::now();
        let error = c
            .acquire_with_wait_clocked(
                &key("sleeper", "shared"),
                std::time::Duration::from_secs(60),
                false,
                |_, _| {},
                &mut clock,
            )
            .unwrap_err();
        assert!(matches!(error, HostResourceError::Conflict { .. }));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(c.last_wait_stats().unwrap().attempts, 1);
        assert!(waiter_rows(&c).is_empty());
    }

    #[test]
    fn re_entry_keeps_the_original_ticket() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&temp.path().join("h.db")).unwrap();
        c.acquire(&capacity("holder", 4)).unwrap();
        let first = capacity("first", 2);
        let ticket = c
            .acquire_attempt(&first, false, Some(&registration()), now_ms())
            .unwrap()
            .ticket
            .unwrap();
        contended(&mut c, &capacity("second", 2));
        // The first waiter's row is lost (as if reaped while it stalled).
        delete_waiter(&c.conn, "first", None).unwrap();
        let mut again = registration();
        again.ticket = Some(ticket);
        let attempt = c
            .acquire_attempt(&first, false, Some(&again), now_ms())
            .unwrap();
        assert_eq!(attempt.ticket, Some(ticket));
        assert_eq!(attempt.queue.position, 0);
        assert_eq!(
            waiter_rows(&c),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    fn wait_for_waiters(coordinator: &HostResourceCoordinator, count: usize) {
        for _ in 0..400 {
            if coordinator.waiting().unwrap().len() >= count {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("{count} waiter(s) never queued");
    }

    #[test]
    fn two_connections_are_served_in_arrival_order() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("h.db");
        let mut main = HostResourceCoordinator::open(&db).unwrap();
        let holder = main.acquire(&capacity("holder", 4)).unwrap();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let spawn = |id: &'static str, units: u32| {
            let db = db.clone();
            let order = order.clone();
            std::thread::spawn(move || {
                let mut c = HostResourceCoordinator::open(&db).unwrap();
                let grant = c
                    .acquire_with_wait(
                        &capacity(id, units),
                        std::time::Duration::from_secs(60),
                        |_| {},
                    )
                    .unwrap();
                order.lock().unwrap().push(id);
                std::thread::sleep(std::time::Duration::from_millis(300));
                release(&mut c, &grant);
            })
        };
        let big = spawn("big", 4);
        wait_for_waiters(&main, 1);
        let small = spawn("small", 2);
        wait_for_waiters(&main, 2);
        release(&mut main, &holder);
        big.join().unwrap();
        small.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["big", "small"]);
        assert!(waiter_rows(&main).is_empty());
    }
}
