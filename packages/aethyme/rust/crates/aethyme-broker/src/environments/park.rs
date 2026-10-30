//! Park and wake (#1212).
//!
//! An environment holds two leases: a reservation (namespace, ports) for its
//! whole life and a capacity lease only while it runs. Parking stops every
//! component, proves it stopped, then releases the capacity lease; waking
//! acquires capacity first (waiting up to `--wait`, or failing clearly) and
//! only then starts components. No lease ever holds a partial allocation and
//! `HostLeaseState` is unchanged.
//!
//! A wake whose start or health check fails stops what it started, releases
//! capacity and returns to `parked` when every stop succeeds. If rollback
//! cannot prove a component stopped, its identity and capacity stay recorded
//! in `quarantined` so `down` can retry safely. A wake that gets no capacity
//! within `--wait` stays `parked` with `capacity_timeout` and starts nothing.
//! A park whose stop cannot be proven also goes to `quarantined` and keeps
//! its capacity.
//!
//! Ports stay reserved while parked, but only Aethyme respects the
//! reservation: another process may bind one in the meantime. A wake checks
//! first and fails with `port_in_use`, naming the port, rather than starting
//! a component that would answer on a different port than its routes.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::drivers::{ComponentIdentity, driver_for, wait_healthy};
use super::exec::{Bindings, DriverError};
use super::lifecycle::{
    CAPACITY_GRANT, Lifecycle, LifecycleError, RESERVATION_GRANT, bindings_from, now_ms,
    read_grant, write_grant,
};
use super::store::{EnvironmentPhase, EnvironmentRecord, StoreError};
use crate::managed_services::ManagedServicePhase;

const SETTLE: [ManagedServicePhase; 3] = [
    ManagedServicePhase::RemovalPending,
    ManagedServicePhase::Removed,
    ManagedServicePhase::Settled,
];

impl Lifecycle {
    fn current(&self, env_id: &str) -> Result<EnvironmentRecord, LifecycleError> {
        Ok(self
            .store
            .get(env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.into()))?)
    }

    pub(super) fn bindings_for(
        &self,
        record: &EnvironmentRecord,
    ) -> Result<Bindings, LifecycleError> {
        let worktree = record
            .worktree_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_default();
        Ok(bindings_from(
            read_grant(&self.root, &record.env_id, RESERVATION_GRANT).as_ref(),
            &worktree,
            self.receipts(&record.env_id)?,
        ))
    }

    /// Stop every component and return capacity to the pool. Idempotent.
    pub fn park(
        &mut self,
        env_id: &str,
        reason: &str,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let _operation = self.operation_lock(&record.env_id)?;
        self.park_locked(&record.env_id, reason)
    }

    pub(super) fn park_locked(
        &mut self,
        env_id: &str,
        reason: &str,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let env_id = record.env_id.clone();
        match record.phase {
            EnvironmentPhase::Parked => return Ok(record),
            EnvironmentPhase::Active => {}
            other => {
                return Err(LifecycleError::WrongPhase {
                    env_id,
                    phase: other.as_str(),
                    next: "only an active environment can be parked".into(),
                });
            }
        }
        let record = self.advance(&record, EnvironmentPhase::Stopping, reason, None)?;
        let snapshot = record.snapshot.clone();
        let bindings = self.bindings_for(&record)?;
        let components = self.store.components(&env_id)?;
        for component in snapshot.components.iter().rev() {
            let identity: Option<ComponentIdentity> = components
                .iter()
                .find(|c| c.component == component.name)
                .and_then(|c| c.identity.clone())
                .and_then(|v| serde_json::from_value(v).ok());
            let ctx = self.context(&snapshot, component, &bindings, 1);
            if let Some(identity) = &identity
                && let Err(error) = driver_for(component.runtime).stop(&ctx, identity)
            {
                // Not proven stopped: never release capacity for something
                // that may still be running.
                let detail = json!({
                    "code": error.code(),
                    "component": component.name,
                    "message": error.to_string(),
                });
                self.store.set_error(&env_id, Some(&detail))?;
                let current = self.current(&env_id)?;
                self.advance(
                    &current,
                    EnvironmentPhase::Quarantined,
                    "stop unproven",
                    Some(detail),
                )?;
                return Err(error.into());
            }
            self.component_to(&env_id, &component.name, &SETTLE, None, None)?;
        }
        self.release_capacity(&env_id)?;
        let current = self.current(&env_id)?;
        self.advance(&current, EnvironmentPhase::Parked, reason, None)
    }

    /// Re-acquire capacity, start every component and wait for health.
    pub fn wake(
        &mut self,
        env_id: &str,
        wait: Duration,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let _operation = self.operation_lock(&record.env_id)?;
        self.wake_locked(&record.env_id, wait)
    }

    pub(super) fn wake_locked(
        &mut self,
        env_id: &str,
        wait: Duration,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let env_id = record.env_id.clone();
        match record.phase {
            EnvironmentPhase::Active => {
                self.store.record_activity(&env_id, now_ms())?;
                return Ok(record);
            }
            EnvironmentPhase::Parked => {}
            other => {
                return Err(LifecycleError::WrongPhase {
                    env_id,
                    phase: other.as_str(),
                    next: "only a parked environment can be woken".into(),
                });
            }
        }
        let snapshot = record.snapshot.clone();
        // Capacity first: a full pool leaves the environment parked with
        // nothing started. The request id is per acquisition.
        if let Some(request) =
            snapshot.capacity_request(self.holder_pid, record.phase_generation as u64)
        {
            match self.coordinator.acquire_with_wait(&request, wait, |_| {}) {
                Ok(grant) => {
                    write_grant(&self.root, &env_id, CAPACITY_GRANT, &grant)?;
                    self.store.set_leases(
                        &env_id,
                        record.reservation_lease_id.as_deref(),
                        Some(&grant.lease.lease_id),
                    )?;
                }
                Err(error) => {
                    let detail = json!({"code": "capacity_timeout", "message": error.to_string()});
                    self.store.set_error(&env_id, Some(&detail))?;
                    return Err(error.into());
                }
            }
        }
        let record = self.advance(&record, EnvironmentPhase::Waking, "wake", None)?;
        let bindings = self.bindings_for(&record)?;
        let mut started: Vec<(String, ComponentIdentity)> = Vec::new();
        let mut failure: Option<(String, DriverError)> = None;
        for component in &snapshot.components {
            if let Some(port) = bindings.ports.get(&component.name)
                && TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, *port)).is_err()
            {
                failure = Some((
                    component.name.clone(),
                    DriverError::Unsupported(format!(
                        "port_in_use: port {port}, reserved for {}, was taken by another process while the environment was parked; run `aethyme env down {env_id}` then `aethyme env up` to reallocate",
                        component.name
                    )),
                ));
                break;
            }
            self.prepare_start(&env_id, &snapshot, &bindings, component)?;
            let ctx = self.context(&snapshot, component, &bindings, 1);
            let driver = driver_for(component.runtime);
            let identity = match driver.start(&ctx) {
                Ok(identity) => identity,
                Err(error) => {
                    failure = Some((component.name.clone(), error));
                    break;
                }
            };
            let identity_json =
                serde_json::to_value(&identity).map_err(|e| LifecycleError::Io(e.to_string()))?;
            self.component_to(
                &env_id,
                &component.name,
                &[ManagedServicePhase::Running],
                Some(&identity_json),
                None,
            )?;
            started.push((component.name.clone(), identity.clone()));
            if let Err(error) = wait_healthy(driver.as_ref(), &ctx, &identity) {
                failure = Some((component.name.clone(), error));
                break;
            }
        }
        let Some((failed, error)) = failure else {
            self.store.set_error(&env_id, None)?;
            self.store.record_activity(&env_id, now_ms())?;
            return self.advance(&record, EnvironmentPhase::Active, "healthy", None);
        };
        // Roll back each started component independently. A failed stop must
        // retain its identity and Running phase for a later `down` retry.
        let mut rollback_errors = Vec::new();
        for (name, identity) in started.iter().rev() {
            if let Some(component) = snapshot.components.iter().find(|c| &c.name == name) {
                let ctx = self.context(&snapshot, component, &bindings, 1);
                match driver_for(component.runtime).stop(&ctx, identity) {
                    Ok(()) => self.component_to(&env_id, name, &SETTLE, None, None)?,
                    Err(stop) => rollback_errors.push(json!({
                        "component": name,
                        "code": stop.code(),
                        "message": stop.to_string(),
                    })),
                }
            }
        }
        if !started.iter().any(|(name, _)| name == &failed) {
            self.component_to(&env_id, &failed, &SETTLE, None, None)?;
        }
        let rollback_failed = !rollback_errors.is_empty();
        if !rollback_failed {
            self.release_capacity(&env_id)?;
        }
        let detail = json!({
            "code": error.code(),
            "component": failed,
            "message": error.to_string(),
            "rollback_errors": rollback_errors,
        });
        self.store.set_error(&env_id, Some(&detail))?;
        let current = self.current(&env_id)?;
        let (phase, reason) = if !rollback_failed {
            (EnvironmentPhase::Parked, "wake failed")
        } else {
            (EnvironmentPhase::Quarantined, "wake rollback stop unproven")
        };
        self.advance(&current, phase, reason, Some(detail))?;
        Err(error.into())
    }
}
