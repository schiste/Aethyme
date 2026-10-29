//! Capacity preemption (#1215): when a `park_lru` pool is full, park the
//! least-recently-active environments instead of waiting.
//!
//! Rules, all hard:
//!
//! - **All or nothing.** Victims are parked only if together they free the
//!   whole shortfall; otherwise nothing is parked and the request waits or
//!   times out exactly as with `preempt = "none"`.
//! - **Never** a victim: a pinned environment, one with a live busy marker
//!   (`env exec` running), a shared (ref-owned) environment, one not
//!   `active`, one active for less than the pool's `min_active_seconds`,
//!   or one with a higher `priority` than the requester.
//! - **Only parks**, never destroys, and only within the same lease pool:
//!   repository pools never preempt across repositories; a host pool may.
//! - **Recorded** on both sides: each victim gets a `preempted` note naming
//!   the beneficiary and the shortfall, and the beneficiary a note listing
//!   its victims.
//!
//! Admission itself is unchanged; this runs before it.

use serde_json::json;

use super::activity::{has_live_busy_marker, last_touch_ms};
use super::compile::{CompiledEnvironment, EnvironmentOwner};
use super::lifecycle::{Lifecycle, LifecycleError};
use super::spec::Preempt;
use super::store::{EnvironmentPhase, EnvironmentRecord};
use crate::resources::HostLeaseState;

impl Lifecycle {
    /// Units held in `lease_pool` by live leases, excluding `except`'s own.
    fn pool_usage(&self, lease_pool: &str, except: &str) -> Result<u32, LifecycleError> {
        Ok(self
            .coordinator
            .list(false)?
            .iter()
            .filter(|lease| lease.state != HostLeaseState::Released && lease.run_id != except)
            .flat_map(|lease| lease.allocations.iter())
            .filter(|allocation| allocation.kind == "capacity" && allocation.value == lease_pool)
            .filter_map(|allocation| allocation.units)
            .sum())
    }

    fn active_since(&self, record: &EnvironmentRecord) -> Result<i64, LifecycleError> {
        Ok(self
            .store
            .transitions(&record.env_id, 200)?
            .iter()
            .rev()
            .find(|t| t.to == EnvironmentPhase::Active && t.from != Some(EnvironmentPhase::Active))
            .map_or(record.updated_at, |t| t.at))
    }

    /// Park victims so `requester` fits in its pool. Returns the parked ids;
    /// empty when no preemption is configured, none is needed, or the
    /// eligible victims could not free the whole shortfall.
    pub fn make_room(
        &mut self,
        requester: &CompiledEnvironment,
        now: i64,
    ) -> Result<Vec<String>, LifecycleError> {
        let Some(pool) = &requester.pool else {
            return Ok(Vec::new());
        };
        if pool.preempt != Preempt::ParkLru {
            return Ok(Vec::new());
        }
        let used = self.pool_usage(&pool.lease_pool, &requester.env_id)?;
        let shortfall = (used + requester.units).saturating_sub(pool.limit);
        if shortfall == 0 {
            return Ok(Vec::new());
        }
        let min_active_ms = pool.min_active_seconds as i64 * 1000;
        let mut candidates = Vec::new();
        for record in self.store.list(None)? {
            let snapshot = &record.snapshot;
            let eligible = record.env_id != requester.env_id
                && record.phase == EnvironmentPhase::Active
                && matches!(record.owner, EnvironmentOwner::Worktree { .. })
                && snapshot
                    .pool
                    .as_ref()
                    .is_some_and(|p| p.lease_pool == pool.lease_pool)
                && snapshot.units > 0
                && snapshot.priority <= requester.priority
                && record.pinned_until.is_none_or(|until| until <= now)
                && !has_live_busy_marker(&self.root, &record.env_id)
                && now - self.active_since(&record)? >= min_active_ms;
            if eligible {
                let last = record
                    .last_activity_at
                    .unwrap_or(record.updated_at)
                    .max(last_touch_ms(&self.root, &record.env_id).unwrap_or(0));
                candidates.push((snapshot.priority, last, record));
            }
        }
        candidates.sort_by_key(|(priority, last, _)| (*priority, *last));
        let mut freed = 0u32;
        let mut victims = Vec::new();
        for (_, _, record) in candidates {
            if freed >= shortfall {
                break;
            }
            freed += record.snapshot.units;
            victims.push(record);
        }
        if freed < shortfall {
            return Ok(Vec::new());
        }
        let mut parked = Vec::new();
        for victim in victims {
            self.park(&victim.env_id, "preempted")?;
            self.store.note(
                &victim.env_id,
                "preempted",
                &self.actor,
                Some(&json!({
                    "beneficiary": requester.env_id,
                    "shortfall_units": shortfall,
                    "pool": pool.name,
                })),
            )?;
            parked.push(victim.env_id);
        }
        if self.store.get(&requester.env_id)?.is_some() {
            self.store.note(
                &requester.env_id,
                "preempted others",
                &self.actor,
                Some(&json!({ "victims": parked, "shortfall_units": shortfall })),
            )?;
        }
        Ok(parked)
    }
}
