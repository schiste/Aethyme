//! Whether a lease's holder is still working (#360).
//!
//! A lease belongs to a session, and a session belongs to the agent process
//! that started or adopted it (#542, `session_holder`). So a lease is as live
//! as that process: running means `active` (or `idle` when it has issued no
//! broker command for a while), gone means `stale`. A stale lease keeps its
//! hold for a visible grace period, so a holder that crashed mid-edit can come
//! back, and only past that grace does it stop counting as a conflict. It is
//! still listed either way.
//!
//! When the broker cannot tell -- no recorded holder, no process table, an
//! unidentified caller -- the lease is `unknown`, and unknown is never read as
//! dead: it keeps whatever hold the session's own status gives it.

use std::path::Path;

/// Default for `[leases] idle_minutes`.
pub const DEFAULT_IDLE_MINUTES: i64 = 30;
/// Default for `[leases] stale_grace_minutes`.
pub const DEFAULT_STALE_GRACE_MINUTES: i64 = 15;

/// Liveness thresholds, from `.aethyme/config.toml` `[leases]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseLivenessPolicy {
    pub idle_ms: i64,
    pub stale_grace_ms: i64,
}

impl Default for LeaseLivenessPolicy {
    fn default() -> Self {
        Self {
            idle_ms: DEFAULT_IDLE_MINUTES * 60_000,
            stale_grace_ms: DEFAULT_STALE_GRACE_MINUTES * 60_000,
        }
    }
}

impl LeaseLivenessPolicy {
    /// Read like the other `[leases]` keys: committed config on the default
    /// branch, else the main checkout's file. A missing config or a negative
    /// threshold keeps the default; an unresolved or syntactically invalid
    /// config conservatively keeps gone leases blocking.
    pub fn load(main_root: &Path) -> Self {
        match crate::merge::repository_config_checked(main_root) {
            Ok(Some((text, _))) => match text.parse::<toml::Value>() {
                Ok(value) => Self::from_value(value),
                Err(_) => Self::with_unresolved_configuration(),
            },
            Ok(None) => Self::default(),
            Err(_) => Self::with_unresolved_configuration(),
        }
    }

    fn with_unresolved_configuration() -> Self {
        Self {
            stale_grace_ms: i64::MAX,
            ..Self::default()
        }
    }

    pub fn from_config_text(text: &str) -> Self {
        let Ok(value) = text.parse::<toml::Value>() else {
            return Self::default();
        };
        Self::from_value(value)
    }

    fn from_value(value: toml::Value) -> Self {
        let mut policy = Self::default();
        let minutes = |key: &str| {
            value
                .get("leases")
                .and_then(|leases| leases.get(key))
                .and_then(toml::Value::as_integer)
                .filter(|minutes| *minutes >= 0)
        };
        if let Some(idle) = minutes("idle_minutes") {
            policy.idle_ms = idle.saturating_mul(60_000);
        }
        if let Some(grace) = minutes("stale_grace_minutes") {
            policy.stale_grace_ms = grace.saturating_mul(60_000);
        }
        policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseLiveness {
    /// The holder process is running and used the broker recently.
    Active,
    /// The holder process is running but quiet past `idle_minutes`.
    /// Informational: an idle lease holds exactly like an active one.
    Idle,
    /// The holder process is gone. Within the grace period the lease still
    /// holds; past it, it is no longer a conflict.
    Stale,
    Released,
    /// Liveness could not be established. Never treated as dead.
    Unknown,
}

impl LeaseLiveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Stale => "stale",
            Self::Released => "released",
            Self::Unknown => "unknown",
        }
    }
}

/// What the liveness verdict rests on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseLivenessEvidence {
    /// One of `holder_running`, `holder_gone`, `released`, `no_recorded_holder`,
    /// `process_table_unavailable`.
    pub basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<i64>,
    /// Latest broker activity of the holding session, epoch ms.
    pub last_activity_at: i64,
    /// When the broker first saw the holder gone, epoch ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<i64>,
    /// Whether the lease still counts as a conflict for another session.
    pub holds: bool,
}

/// One lease with its liveness, as `broker status --json` and
/// `leases explain` report it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseLivenessView {
    pub lease_id: i64,
    pub session_id: i64,
    pub path: String,
    pub kind: crate::LeaseKind,
    pub liveness: LeaseLiveness,
    pub liveness_evidence: LeaseLivenessEvidence,
}

/// What the broker observed about a lease's holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderObservation {
    Running,
    /// Gone; `since` is when the broker first saw it gone (epoch ms).
    Gone {
        since: i64,
    },
    NoRecordedHolder,
    ProcessTableUnavailable,
}

/// Classify one lease. Pure, so every state is testable without processes.
pub fn classify(
    released: bool,
    holder: &HolderObservation,
    holder_pid: Option<i64>,
    last_activity_at: i64,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
) -> (LeaseLiveness, LeaseLivenessEvidence) {
    let evidence = |basis, stale_since, grace_ends_at, holds| LeaseLivenessEvidence {
        basis,
        holder_pid,
        last_activity_at,
        stale_since,
        grace_ends_at,
        holds,
    };
    if released {
        return (
            LeaseLiveness::Released,
            evidence("released", None, None, false),
        );
    }
    match holder {
        HolderObservation::Running => {
            let quiet = now_ms.saturating_sub(last_activity_at) > policy.idle_ms;
            let liveness = if quiet {
                LeaseLiveness::Idle
            } else {
                LeaseLiveness::Active
            };
            (liveness, evidence("holder_running", None, None, true))
        }
        HolderObservation::Gone { since } => {
            let grace_ends_at = since.saturating_add(policy.stale_grace_ms);
            (
                LeaseLiveness::Stale,
                evidence(
                    "holder_gone",
                    Some(*since),
                    Some(grace_ends_at),
                    now_ms < grace_ends_at,
                ),
            )
        }
        HolderObservation::NoRecordedHolder => (
            LeaseLiveness::Unknown,
            evidence("no_recorded_holder", None, None, true),
        ),
        HolderObservation::ProcessTableUnavailable => (
            LeaseLiveness::Unknown,
            evidence("process_table_unavailable", None, None, true),
        ),
    }
}

/// What the broker can see of a session's holder right now, without writing.
/// A holder seen gone for the first time reads as gone since `now_ms`; the
/// grace clock only advances once [`record_gone_holders`] has persisted it.
pub fn observe_holder(
    store: &crate::BrokerStore,
    session_id: i64,
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
) -> Result<(HolderObservation, Option<i64>), crate::BrokerOpError> {
    observe_holder_for_cleanup(store, session_id, table, now_ms, false)
}

fn observe_holder_for_cleanup(
    store: &crate::BrokerStore,
    session_id: i64,
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
    abandoned_session: bool,
) -> Result<(HolderObservation, Option<i64>), crate::BrokerOpError> {
    let Some(holder) = crate::session_holder::recorded_holder(store, session_id)? else {
        return Ok((HolderObservation::NoRecordedHolder, None));
    };
    if crate::session_holder::is_shared_agent_service(&holder.command) {
        // A legacy shared-service binding remains an ownership record, but
        // service uptime proves neither this task's activity nor its death.
        if !abandoned_session {
            return Ok((HolderObservation::NoRecordedHolder, Some(holder.pid)));
        }
        let Some(table) = table else {
            return Ok((HolderObservation::ProcessTableUnavailable, Some(holder.pid)));
        };
        // An absent daemon alone is not evidence that this task is abandoned.
        // Require a persisted gone event for this exact binding, then apply
        // the ordinary lease grace below through `classify`.
        if table.is_alive(&holder) {
            return Ok((HolderObservation::NoRecordedHolder, Some(holder.pid)));
        }
        return Ok((
            match recorded_gone_since(store, session_id, &holder)? {
                Some(since) => HolderObservation::Gone { since },
                None => HolderObservation::NoRecordedHolder,
            },
            Some(holder.pid),
        ));
    }
    let Some(table) = table else {
        return Ok((HolderObservation::ProcessTableUnavailable, Some(holder.pid)));
    };
    if table.is_alive(&holder) {
        return Ok((HolderObservation::Running, Some(holder.pid)));
    }
    let since = recorded_gone_since(store, session_id, &holder)?.unwrap_or(now_ms);
    Ok((HolderObservation::Gone { since }, Some(holder.pid)))
}

/// When the broker first recorded this exact holder gone, if it has. A gone
/// event for an earlier holder, or one recorded before the current binding,
/// does not count.
fn recorded_gone_since(
    store: &crate::BrokerStore,
    session_id: i64,
    holder: &crate::session_holder::AgentProcess,
) -> Result<Option<i64>, crate::BrokerOpError> {
    let Some(gone) = store.latest_session_holder_gone_event(session_id)? else {
        return Ok(None);
    };
    let bound_id = store
        .latest_session_holder_event(session_id)?
        .map(|event| event.id)
        .unwrap_or(0);
    let same_holder = gone
        .payload_json
        .as_deref()
        .and_then(|payload| {
            serde_json::from_str::<crate::session_holder::AgentProcess>(payload).ok()
        })
        .is_some_and(|recorded| &recorded == holder);
    Ok((gone.id > bound_id && same_holder).then_some(gone.ts))
}

/// Persist the first sighting of each live session's holder being gone, so
/// the stale grace runs from that moment rather than restarting on every
/// read. Returns the sessions recorded now.
pub fn record_gone_holders(
    store: &mut crate::BrokerStore,
    session_ids: &[i64],
    table: &crate::session_holder::ProcessTable,
) -> Result<Vec<i64>, crate::BrokerOpError> {
    let mut recorded = Vec::new();
    for &session_id in session_ids {
        let Some(holder) = crate::session_holder::recorded_holder(store, session_id)? else {
            continue;
        };
        if table.is_alive(&holder) || recorded_gone_since(store, session_id, &holder)?.is_some() {
            continue;
        }
        store.append_event(
            crate::events::SESSION_HOLDER_GONE,
            Some(session_id),
            Some(&crate::events::session_holder_gone_payload(&holder)),
        )?;
        recorded.push(session_id);
    }
    Ok(recorded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoneHolderWrite {
    Skipped,
    Recorded,
    DatabaseBusy,
}

/// Persist exact absence observed by the final lease check. The caller
/// supplies the process table captured for this binding.
pub(crate) fn record_gone_holder_for_abandoned_session(
    store: &mut crate::BrokerStore,
    session_id: i64,
    binding_event_id: i64,
    holder: &crate::session_holder::AgentProcess,
    table: Option<&crate::session_holder::ProcessTable>,
    deadline: Option<std::time::Instant>,
) -> Result<GoneHolderWrite, crate::BrokerOpError> {
    let Some(table) = table else {
        return Ok(GoneHolderWrite::Skipped);
    };
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        || table.is_alive(holder)
    {
        return Ok(GoneHolderWrite::Skipped);
    }
    match store.record_session_holder_gone_for_binding(session_id, binding_event_id, holder) {
        Ok(true) => Ok(GoneHolderWrite::Recorded),
        Ok(false) => Ok(GoneHolderWrite::Skipped),
        Err(crate::BrokerError::SessionRetirementDatabaseBusy) => Ok(GoneHolderWrite::DatabaseBusy),
        Err(error) => Err(error.into()),
    }
}

/// Liveness of every given lease, one holder lookup per session.
pub fn assess(
    store: &crate::BrokerStore,
    leases: &[crate::Lease],
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
) -> Result<Vec<LeaseLivenessView>, crate::BrokerOpError> {
    assess_for_cleanup(store, leases, table, now_ms, policy, false)
}

/// Only the automatic abandonment path may use exact dead shared-service
/// evidence. Ordinary claim/status consumers retain conservative semantics.
pub(crate) fn assess_for_abandonment(
    store: &crate::BrokerStore,
    leases: &[crate::Lease],
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
) -> Result<Vec<LeaseLivenessView>, crate::BrokerOpError> {
    assess_for_cleanup(store, leases, table, now_ms, policy, true)
}

fn assess_for_cleanup(
    store: &crate::BrokerStore,
    leases: &[crate::Lease],
    table: Option<&crate::session_holder::ProcessTable>,
    now_ms: i64,
    policy: LeaseLivenessPolicy,
    abandoned_session: bool,
) -> Result<Vec<LeaseLivenessView>, crate::BrokerOpError> {
    let mut holders = std::collections::HashMap::new();
    let mut views = Vec::with_capacity(leases.len());
    for lease in leases {
        let ((observation, pid), activity) = match holders.entry(lease.session_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert((
                observe_holder_for_cleanup(
                    store,
                    lease.session_id,
                    table,
                    now_ms,
                    abandoned_session,
                )?,
                store.session(lease.session_id)?.last_activity_at,
            )),
        };
        let (liveness, liveness_evidence) = classify(
            lease.released_at.is_some(),
            observation,
            *pid,
            *activity,
            now_ms,
            policy,
        );
        views.push(LeaseLivenessView {
            lease_id: lease.id,
            session_id: lease.session_id,
            path: lease.path.clone(),
            kind: lease.kind,
            liveness,
            liveness_evidence,
        });
    }
    Ok(views)
}

/// Sessions whose holder is gone past the grace: their leases no longer
/// count as conflicts.
pub fn released_by_grace(views: &[LeaseLivenessView]) -> std::collections::HashSet<i64> {
    views
        .iter()
        .filter(|view| view.liveness == LeaseLiveness::Stale && !view.liveness_evidence.holds)
        .map(|view| view.session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    fn policy() -> LeaseLivenessPolicy {
        LeaseLivenessPolicy {
            idle_ms: 30 * MINUTE,
            stale_grace_ms: 15 * MINUTE,
        }
    }

    fn shared_holder_fixture() -> (
        tempfile::TempDir,
        crate::BrokerStore,
        i64,
        crate::session_holder::AgentProcess,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&dir.path().join("broker.db")).unwrap();
        let session = store
            .register_session(&crate::NewSession {
                worktree_path: "/tmp/shared-holder-fixture".into(),
                branch: "agent/shared-holder-fixture".into(),
                origin: crate::SessionOrigin::Adopted,
                task: None,
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap();
        let holder = crate::session_holder::AgentProcess {
            pid: 3731,
            started: "Fri Oct 23 08:42:41 2026".into(),
            command: "/opt/codex app-server --listen local".into(),
        };
        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        (dir, store, session.id, holder)
    }

    fn bind_holder(
        store: &mut crate::BrokerStore,
        session_id: i64,
        holder: &crate::session_holder::AgentProcess,
    ) -> i64 {
        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session_id),
                Some(&crate::events::session_holder_bound_payload(
                    holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        store
            .latest_session_holder_event(session_id)
            .unwrap()
            .unwrap()
            .id
    }

    #[test]
    fn abandoned_lease_check_records_gone_once_and_rebind_starts_a_new_grace() {
        let (_dir, mut store, id, mut holder) = shared_holder_fixture();
        holder.pid = i64::MAX;
        holder.started = "Mon Oct 26 08:42:41 2026".into();
        let binding = bind_holder(&mut store, id, &holder);
        let absent = crate::session_holder::ProcessTable::default();

        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                binding,
                &holder,
                Some(&absent),
                None,
            )
            .unwrap(),
            GoneHolderWrite::Recorded
        );
        let first_gone = store.latest_session_holder_gone_event(id).unwrap().unwrap();
        assert!(first_gone.id > binding);
        let alternate_serialization = serde_json::to_string_pretty(&holder).unwrap();
        store
            .connection()
            .execute(
                "UPDATE events SET payload_json = ?1 WHERE id = ?2",
                rusqlite::params![alternate_serialization, first_gone.id],
            )
            .unwrap();
        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                binding,
                &holder,
                Some(&absent),
                None,
            )
            .unwrap(),
            GoneHolderWrite::Skipped
        );
        let repeated_gone = store.latest_session_holder_gone_event(id).unwrap().unwrap();
        assert_eq!(
            (repeated_gone.id, repeated_gone.ts),
            (first_gone.id, first_gone.ts)
        );

        let rebound = crate::session_holder::AgentProcess {
            pid: i64::MAX - 1,
            started: "Tue Oct 27 08:42:41 2026".into(),
            command: holder.command.clone(),
        };
        let rebound_binding = bind_holder(&mut store, id, &rebound);
        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                rebound_binding,
                &rebound,
                Some(&absent),
                None,
            )
            .unwrap(),
            GoneHolderWrite::Recorded
        );
        let rebound_gone = store.latest_session_holder_gone_event(id).unwrap().unwrap();
        assert!(rebound_gone.id > rebound_binding);
        assert_eq!(
            recorded_gone_since(&store, id, &rebound).unwrap(),
            Some(rebound_gone.ts)
        );
        assert_eq!(
            recorded_gone_since(&store, id, &holder).unwrap(),
            None,
            "the previous binding's gone evidence does not survive a rebind"
        );
    }

    #[test]
    fn gone_holder_recorder_preserves_unknown_live_and_unavailable_observations() {
        let (_dir, mut store, id, mut holder) = shared_holder_fixture();
        holder.pid = i64::MAX;
        holder.started = "Mon Oct 26 08:42:41 2026".into();
        let binding = bind_holder(&mut store, id, &holder);

        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                binding,
                &holder,
                Some(&crate::session_holder::ProcessTable::default()),
                Some(std::time::Instant::now() - std::time::Duration::from_millis(1)),
            )
            .unwrap(),
            GoneHolderWrite::Skipped,
            "an expired retirement deadline cannot start lease grace"
        );
        assert!(
            store
                .latest_session_holder_gone_event(id)
                .unwrap()
                .is_none()
        );

        assert_eq!(
            record_gone_holder_for_abandoned_session(&mut store, id, binding, &holder, None, None,)
                .unwrap(),
            GoneHolderWrite::Skipped,
            "an unavailable or failed process snapshot cannot start lease grace"
        );
        assert!(
            store
                .latest_session_holder_gone_event(id)
                .unwrap()
                .is_none()
        );

        let live = crate::session_holder::ProcessTable::parse(&format!(
            "{} 1 {} {}\n",
            holder.pid, holder.started, holder.command
        ));
        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                binding,
                &holder,
                Some(&live),
                None,
            )
            .unwrap(),
            GoneHolderWrite::Skipped,
            "a live exact holder must not be recorded gone"
        );
        assert!(
            store
                .latest_session_holder_gone_event(id)
                .unwrap()
                .is_none()
        );

        let rebound = crate::session_holder::AgentProcess {
            pid: i64::MAX - 1,
            started: "Tue Oct 27 08:42:41 2026".into(),
            command: holder.command.clone(),
        };
        bind_holder(&mut store, id, &rebound);
        let absent = crate::session_holder::ProcessTable::default();
        assert_eq!(
            record_gone_holder_for_abandoned_session(
                &mut store,
                id,
                binding,
                &holder,
                Some(&absent),
                None,
            )
            .unwrap(),
            GoneHolderWrite::Skipped,
            "the conditional write rejects a binding that changed after observation"
        );
        assert!(
            store
                .latest_session_holder_gone_event(id)
                .unwrap()
                .is_none()
        );

        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(id),
                Some("not-json"),
            )
            .unwrap();
        assert_eq!(
            crate::session_holder::recorded_holder_liveness(
                &store,
                id,
                &mut crate::session_holder::RecordedHolderProcessTable::default(),
                false,
            )
            .liveness,
            crate::session_holder::RecordedHolderLiveness::Unknown,
            "an unreadable binding remains unknown and cannot reach the recorder"
        );
        assert!(
            store
                .latest_session_holder_gone_event(id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn abandonment_shared_holder_needs_exact_persisted_gone_and_complete_absent_snapshot() {
        let (_dir, mut store, id, holder) = shared_holder_fixture();
        let absent = crate::session_holder::ProcessTable::default();
        let live = crate::session_holder::ProcessTable::parse(
            "3731 1 Fri Oct 23 08:42:41 2026 /opt/codex app-server --listen local\n",
        );
        assert_eq!(
            observe_holder_for_cleanup(&store, id, Some(&absent), 0, true)
                .unwrap()
                .0,
            HolderObservation::NoRecordedHolder
        );
        let mut wrong = holder.clone();
        wrong.started = "different start".into();
        store
            .append_event(
                crate::events::SESSION_HOLDER_GONE,
                Some(id),
                Some(&crate::events::session_holder_gone_payload(&wrong)),
            )
            .unwrap();
        assert_eq!(
            observe_holder_for_cleanup(&store, id, Some(&absent), 0, true)
                .unwrap()
                .0,
            HolderObservation::NoRecordedHolder
        );
        store
            .append_event(
                crate::events::SESSION_HOLDER_GONE,
                Some(id),
                Some(&crate::events::session_holder_gone_payload(&holder)),
            )
            .unwrap();
        let since = store
            .latest_session_holder_gone_event(id)
            .unwrap()
            .unwrap()
            .ts;
        assert_eq!(
            observe_holder_for_cleanup(&store, id, None, since, true)
                .unwrap()
                .0,
            HolderObservation::ProcessTableUnavailable
        );
        assert_eq!(
            observe_holder_for_cleanup(&store, id, Some(&live), since, true)
                .unwrap()
                .0,
            HolderObservation::NoRecordedHolder
        );
        let observation = observe_holder_for_cleanup(&store, id, Some(&absent), since, true)
            .unwrap()
            .0;
        assert_eq!(observation, HolderObservation::Gone { since });
        assert!(
            classify(false, &observation, Some(holder.pid), 0, since, policy())
                .1
                .holds
        );
        assert!(
            !classify(
                false,
                &observation,
                Some(holder.pid),
                0,
                since + policy().stale_grace_ms,
                policy()
            )
            .1
            .holds
        );
        let lease = store.claim_lease(id, "source.rs", None).unwrap();
        assert!(
            assess(
                &store,
                std::slice::from_ref(&lease),
                Some(&absent),
                since + policy().stale_grace_ms,
                policy()
            )
            .unwrap()[0]
                .liveness_evidence
                .holds
        );
        assert!(
            !assess_for_abandonment(
                &store,
                &[lease],
                Some(&absent),
                since + policy().stale_grace_ms,
                policy()
            )
            .unwrap()[0]
                .liveness_evidence
                .holds
        );
        // Rebinding even the same PID/start requires a newer gone event.
        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        assert_eq!(
            observe_holder_for_cleanup(&store, id, Some(&absent), since, true)
                .unwrap()
                .0,
            HolderObservation::NoRecordedHolder
        );
    }

    #[test]
    fn a_running_holder_is_active_then_idle_and_always_holds() {
        let now = 100 * MINUTE;
        let (fresh, evidence) = classify(
            false,
            &HolderObservation::Running,
            Some(7),
            now - MINUTE,
            now,
            policy(),
        );
        assert_eq!(fresh, LeaseLiveness::Active);
        assert!(evidence.holds);
        let (quiet, evidence) = classify(
            false,
            &HolderObservation::Running,
            Some(7),
            now - 31 * MINUTE,
            now,
            policy(),
        );
        assert_eq!(quiet, LeaseLiveness::Idle);
        assert!(evidence.holds, "idle is informational and keeps the hold");
    }

    #[test]
    fn a_gone_holder_holds_until_the_grace_ends() {
        let since = 50 * MINUTE;
        let gone = HolderObservation::Gone { since };
        let (within, evidence) = classify(false, &gone, Some(7), 0, since + MINUTE, policy());
        assert_eq!(within, LeaseLiveness::Stale);
        assert!(evidence.holds);
        assert_eq!(evidence.stale_since, Some(since));
        assert_eq!(evidence.grace_ends_at, Some(since + 15 * MINUTE));
        let (past, evidence) = classify(false, &gone, Some(7), 0, since + 15 * MINUTE, policy());
        assert_eq!(past, LeaseLiveness::Stale);
        assert!(
            !evidence.holds,
            "past the grace a stale lease no longer holds"
        );
    }

    #[test]
    fn unknown_liveness_is_never_treated_as_dead() {
        for observation in [
            HolderObservation::NoRecordedHolder,
            HolderObservation::ProcessTableUnavailable,
        ] {
            let (liveness, evidence) =
                classify(false, &observation, None, 0, i64::MAX / 2, policy());
            assert_eq!(liveness, LeaseLiveness::Unknown);
            assert!(evidence.holds, "{observation:?} must keep the hold");
        }
    }

    #[test]
    fn a_released_lease_is_released_whatever_the_holder() {
        let (liveness, evidence) =
            classify(true, &HolderObservation::Running, Some(7), 0, 0, policy());
        assert_eq!(liveness, LeaseLiveness::Released);
        assert!(!evidence.holds);
    }

    #[test]
    fn ambiguous_configuration_keeps_gone_leases_blocking() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join(".aethyme");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[leases]\nstale_grace_minutes = 360\n",
        )
        .unwrap();
        std::fs::write(
            config.join("config.yaml"),
            "leases:\n  stale_grace_minutes: 0\n",
        )
        .unwrap();

        let policy = LeaseLivenessPolicy::load(root.path());
        assert_eq!(policy.stale_grace_ms, i64::MAX);
        let (_, evidence) = classify(
            false,
            &HolderObservation::Gone { since: 0 },
            Some(7),
            0,
            i64::MAX / 2,
            policy,
        );
        assert!(evidence.holds);
    }

    #[test]
    fn config_keys_set_the_thresholds_and_bad_values_keep_defaults() {
        let policy = LeaseLivenessPolicy::from_config_text(
            "[leases]\nidle_minutes = 5\nstale_grace_minutes = 0\n",
        );
        assert_eq!(policy.idle_ms, 5 * MINUTE);
        assert_eq!(policy.stale_grace_ms, 0);
        let fallback = LeaseLivenessPolicy::from_config_text(
            "[leases]\nidle_minutes = -1\nstale_grace_minutes = \"x\"\n",
        );
        assert_eq!(fallback, LeaseLivenessPolicy::default());
        assert_eq!(
            LeaseLivenessPolicy::from_config_text("not toml ["),
            LeaseLivenessPolicy::default()
        );
    }
}
